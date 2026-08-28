//! Reading and writing files that another process could be racing us for.
//!
//! Every read is capped before it allocates and validated through the same
//! descriptor it will read from, so a path cannot be swapped for something else
//! between the check and the read. Every write lands via an exclusive temporary
//! file in the same directory followed by a rename, so a reader never observes a
//! half-written file and a failure leaves the original untouched.

use anyhow::{anyhow, bail, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// `O_NOFOLLOW` refuses to open the file if the final component is a symlink,
/// which is the swap an attacker would use to redirect a privileged write.
fn open_no_follow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| anyhow!("cannot open {}: {e}", path.display()))
}

/// Validates through the already-open descriptor rather than the path, so the
/// answer cannot change between the check and the read.
fn verify_regular_and_owned(file: &File, path: &Path) -> Result<u64> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    // SAFETY: getuid is always safe and cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid {
        bail!("{} is owned by uid {}, not {uid}", path.display(), meta.uid());
    }
    Ok(meta.len())
}

/// Reads at most `limit` bytes, refusing anything larger instead of truncating,
/// so a caller never silently parses a fragment of a bigger file.
pub fn read_capped(path: &Path, limit: u64) -> Result<String> {
    let file = open_no_follow(path)?;
    let size = verify_regular_and_owned(&file, path)?;
    if size > limit {
        bail!("{} is {size} bytes, over the {limit} byte limit", path.display());
    }

    let mut buffer = Vec::with_capacity(size.min(limit) as usize + 1);
    // Reading through `take` bounds the allocation even if the file grew after
    // the size check above.
    file.take(limit + 1).read_to_end(&mut buffer)?;
    if buffer.len() as u64 > limit {
        bail!("{} grew past the {limit} byte limit while being read", path.display());
    }
    Ok(String::from_utf8(buffer)?)
}

/// Same as [`read_capped`], but a missing file is simply absent content.
pub fn read_capped_optional(path: &Path, limit: u64) -> Option<String> {
    read_capped(path, limit).ok()
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    // Only meaningful for directories we just created; tightening an existing
    // one the user chose is not ours to do.
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() && meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 != 0 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn temp_sibling(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{name}.{}.{nonce}.tmp", std::process::id())))
}

/// Writes `contents` to `path` atomically, preserving the existing mode.
///
/// The temporary file is created `O_EXCL` in the same directory — same
/// filesystem, so the rename is atomic — and the rename replaces the target
/// without following a symlink that may have been put there. If anything fails,
/// the temporary file is removed and the original is left exactly as it was.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    ensure_private_dir(parent)?;

    // Inherit the existing permissions so an atomic rewrite never widens access.
    // symlink_metadata: a symlink at the target must not redirect this lookup.
    let mode = std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.mode() & 0o7777)
        .unwrap_or(0o600);

    let temp = temp_sibling(path)?;
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_CLOEXEC)
            .open(&temp)?;
        file.write_all(contents.as_bytes())?;
        // Without this the rename can be durable while the contents are not.
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omarchycast-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_within_the_cap_and_refuses_beyond_it() {
        let dir = scratch("cap");
        let path = dir.join("f");
        std::fs::write(&path, "hello").unwrap();
        assert_eq!(read_capped(&path, 1024).unwrap(), "hello");
        assert!(read_capped(&path, 4).is_err(), "should refuse a file over the cap");
    }

    #[test]
    fn refuses_to_follow_a_symlink() {
        let dir = scratch("symlink");
        let real = dir.join("real");
        std::fs::write(&real, "secret").unwrap();
        let link = dir.join("link");
        symlink(&real, &link).unwrap();
        assert!(read_capped(&link, 1024).is_err(), "O_NOFOLLOW should refuse the symlink");
    }

    #[test]
    fn refuses_a_directory() {
        let dir = scratch("dir");
        assert!(read_capped(&dir, 1024).is_err());
    }

    #[test]
    fn atomic_write_replaces_content_and_keeps_mode() {
        let dir = scratch("atomic");
        let path = dir.join("cfg");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        write_atomic(&path, "new").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o640, "existing permissions must be preserved");
        // No temporary files may be left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files were left behind");
    }

    #[test]
    fn a_new_file_is_private_by_default() {
        let dir = scratch("newfile");
        let path = dir.join("fresh");
        write_atomic(&path, "x").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    }
}

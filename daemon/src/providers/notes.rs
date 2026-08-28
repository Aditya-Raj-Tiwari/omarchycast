//! Markdown notes, opened in the shadow-notes popup.
//!
//! Titles are matched fuzzily and body text literally: a fuzzy match against a
//! whole document scores almost anything, which buries the notes you meant.

use crate::core::score::{combine, pattern, score};
use crate::core::{Action, Item, Provider, Query};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const PROVIDER: &str = "note";
const KEYWORD: &str = "note";
const VIEWER: &str = "omacastnotes";
/// Only the start of a note is searched; nobody looks for a note by its tail.
const BODY_WINDOW: usize = 8192;

pub fn default_directory() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Notes")
}

/// The single scratchpad omacastnotes opens when given no argument. It is
/// listed alongside real notes so the launcher can reach it too. The old
/// shadow-notes location still counts, for installs from before the rename.
fn scratchpad() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    for dir in ["omacastnotes", "shadow-notes"] {
        let path = home.join(format!(".local/share/{dir}/data/notes.md"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

struct Note {
    path: PathBuf,
    title: String,
    subtitle: String,
    body_lower: String,
}

pub struct NotesProvider {
    directory: RwLock<PathBuf>,
    notes: RwLock<Vec<Note>>,
}

/// First `# heading`, else a readable form of the filename.
fn title_for(path: &Path, contents: &str) -> String {
    for line in contents.lines().take(20) {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix("# ") {
            let heading = heading.trim();
            if !heading.is_empty() {
                return heading.to_string();
            }
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|stem| stem.replace(['-', '_'], " "))
        .unwrap_or_else(|| "Untitled".to_string())
}

/// The first non-heading, non-empty line, which is what the note is actually about.
fn preview_for(contents: &str) -> String {
    for line in contents.lines().take(40) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() > 90 {
            return collapsed.chars().take(89).collect::<String>() + "…";
        }
        return collapsed;
    }
    String::new()
}

fn read_notes(directory: &Path) -> Vec<Note> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten().take(crate::limits::MAX_NOTES) {
            let path = entry.path();
            let is_markdown = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
            if is_markdown && path.is_file() {
                paths.push(path);
            }
        }
    }
    if let Some(pad) = scratchpad() {
        if !paths.contains(&pad) {
            paths.push(pad);
        }
    }

    paths
        .into_iter()
        .filter_map(|path| {
            // Capped, owner-checked, symlink-refusing read: a notes directory
            // is user data, and one 10 GB file must not become a 10 GB buffer.
            let contents =
                crate::safeio::read_capped_optional(&path, crate::limits::MAX_NOTE_BYTES)?;
            let window: String = contents.chars().take(BODY_WINDOW).collect();
            Some(Note {
                title: title_for(&path, &contents),
                subtitle: preview_for(&contents),
                body_lower: window.to_lowercase(),
                path,
            })
        })
        .collect()
}

/// A filename that is safe, readable and stable for a given title.
fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug.chars().take(60).collect()
    }
}

impl NotesProvider {
    pub fn new(directory: PathBuf) -> Arc<Self> {
        let provider =
            Arc::new(NotesProvider { directory: RwLock::new(directory), notes: RwLock::new(Vec::new()) });
        provider.reindex();
        provider
    }

    pub fn directory(&self) -> PathBuf {
        self.directory.read().map(|d| d.clone()).unwrap_or_else(|_| default_directory())
    }

    pub fn set_directory(&self, directory: PathBuf) {
        if let Ok(mut current) = self.directory.write() {
            if *current == directory {
                return;
            }
            *current = directory;
        }
        self.reindex();
    }

    /// Creates the note, then opens it, so capture is a single keystroke.
    fn create(&self, title: &str) -> Result<PathBuf> {
        let title = title.trim();
        if title.is_empty() {
            return Err(anyhow!("a note needs a title"));
        }
        let directory = self.directory();
        std::fs::create_dir_all(&directory)?;

        let mut path = directory.join(format!("{}.md", slugify(title)));
        // Never clobber an existing note with the same title.
        let mut suffix = 2;
        while path.exists() {
            path = directory.join(format!("{}-{suffix}.md", slugify(title)));
            suffix += 1;
        }
        crate::safeio::write_atomic(&path, &format!("# {title}\n\n"))?;
        self.reindex();
        Ok(path)
    }
}

impl Provider for NotesProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        let capture = q.trimmed.strip_prefix(KEYWORD).and_then(|rest| {
            rest.strip_prefix(' ').map(str::trim_start).filter(|t| !t.is_empty())
        });

        let mut items = Vec::new();

        // `note <title>` offers to create, pinned above the notes it matches.
        if let Some(title) = capture {
            let mut item = Item::new(PROVIDER, "Note", format!("new:{title}"), format!("New note: {title}"));
            item.subtitle = Some(format!("Create in {}", self.directory().display()));
            item.glyph = Some("\u{270e}".to_string());
            item.score = 900_000;
            items.push(item);
        }

        let needle = capture.map(str::to_string).unwrap_or_else(|| q.trimmed.clone());
        if needle.chars().count() < 2 {
            return items;
        }

        let notes = match self.notes.read() {
            Ok(n) => n,
            Err(_) => return items,
        };
        let pat = pattern(&needle);
        let body_needle = needle.to_lowercase();

        for note in notes.iter() {
            let title_score = score(&pat, &note.title);
            // A body hit is real but weaker evidence than a title hit.
            let body_hit = note.body_lower.contains(&body_needle);
            let Some(relevance) = title_score.map(|s| combine(s, 0)).or(body_hit.then_some(20_000))
            else {
                continue;
            };

            let mut item = Item::new(
                PROVIDER,
                "Note",
                note.path.to_string_lossy().to_string(),
                note.title.clone(),
            );
            item.subtitle = (!note.subtitle.is_empty()).then(|| note.subtitle.clone());
            item.glyph = Some("\u{1f5c9}".to_string());
            item.score = relevance;
            items.push(item);
        }
        items
    }

    fn activate(&self, id: &str, _action: Action) -> Result<()> {
        let path = match id.strip_prefix("new:") {
            Some(title) => self.create(title)?,
            None => PathBuf::from(id),
        };
        crate::launch::detached(VIEWER, &[path.as_os_str()])
    }

    fn reindex(&self) {
        let fresh = read_notes(&self.directory());
        if let Ok(mut notes) = self.notes.write() {
            *notes = fresh;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_safe_and_readable() {
        assert_eq!(slugify("Meeting notes: Q3 / roadmap"), "meeting-notes-q3-roadmap");
        assert_eq!(slugify("  ...  "), "note");
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
    }

    #[test]
    fn title_prefers_the_first_heading() {
        assert_eq!(title_for(Path::new("/x/a-note.md"), "\n# Real Title\nbody"), "Real Title");
        assert_eq!(title_for(Path::new("/x/a-note.md"), "no heading here"), "a note");
    }

    #[test]
    fn preview_skips_headings_and_blanks() {
        assert_eq!(preview_for("# Title\n\n\nThe   body line\nmore"), "The body line");
        assert_eq!(preview_for("# Title\n"), "");
    }
}

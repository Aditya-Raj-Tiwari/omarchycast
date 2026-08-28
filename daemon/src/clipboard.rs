//! Thin wrappers over `wl-copy`. It forks into the background to keep serving the
//! selection, so the process we spawn here exits almost immediately.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy_text(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .arg("--type")
        .arg("text/plain;charset=utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("wl-copy not available: {e}"))?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("wl-copy stdin unavailable"))?
        .write_all(text.as_bytes())?;

    child.wait()?;
    Ok(())
}


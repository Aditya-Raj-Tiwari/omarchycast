//! Hotkey installation.
//!
//! Wayland gives no client-side global hotkey, so the binding has to live in the
//! compositor's config. We own one small file and add a single `dofile` line to
//! `hyprland.lua` — the same pattern `hyprmoncfg` uses — so the change is obvious
//! and trivially reversible.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

pub const PLUGIN_ID: &str = "io.github.aditya-raj-tiwari.omarchycast";
const MARKER: &str = "-- Added by omarchycast:";

fn hypr_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".config/hypr");
    dir.is_dir().then_some(dir)
}

fn binding_file() -> Option<PathBuf> {
    Some(hypr_dir()?.join("omarchycast.lua"))
}

/// Rejects anything that isn't a plain `MOD + KEY` combination, so a settings
/// value can never turn into arbitrary Lua inside the user's config.
fn validate_hotkey(hotkey: &str) -> Result<String> {
    let cleaned: String = hotkey.trim().to_uppercase();
    if cleaned.is_empty() || cleaned.len() > 64 {
        return Err(anyhow!("hotkey must be between 1 and 64 characters"));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == ' ' || c == '_';
    if !cleaned.chars().all(allowed) {
        return Err(anyhow!("hotkey may only contain letters, digits, '+' and spaces"));
    }
    Ok(cleaned)
}

/// Writes the binding file and makes sure `hyprland.lua` loads it.
pub fn install_hotkey(hotkey: &str) -> Result<()> {
    let hotkey = validate_hotkey(hotkey)?;
    let path = binding_file().ok_or_else(|| anyhow!("~/.config/hypr does not exist"))?;

    let contents = format!(
        "-- Managed by Omarchycast. This file is rewritten whenever the hotkey is\n\
         -- changed from the launcher's settings panel, so edit it there instead.\n\
         pcall(function() hl.unbind(\"{hotkey}\") end)\n\
         o.bind(\"{hotkey}\", \"Omarchycast\", \"omarchy-shell shell toggle {PLUGIN_ID}\")\n"
    );
    std::fs::write(&path, contents)?;

    ensure_sourced(&path)?;
    reload();
    Ok(())
}

/// Appends the `dofile` line to `hyprland.lua` exactly once.
fn ensure_sourced(binding_path: &PathBuf) -> Result<()> {
    let config = hypr_dir()
        .map(|d| d.join("hyprland.lua"))
        .ok_or_else(|| anyhow!("~/.config/hypr does not exist"))?;
    let existing = std::fs::read_to_string(&config).unwrap_or_default();
    if existing.contains(MARKER) {
        return Ok(());
    }
    let _ = binding_path;
    let line = format!(
        "\n{MARKER} loads the launcher hotkey. Remove these two lines to uninstall it.\n\
         dofile(os.getenv(\"HOME\") .. \"/.config/hypr/omarchycast.lua\")\n"
    );
    let updated = format!("{}{}", existing.trim_end_matches('\n'), line);
    std::fs::write(&config, updated)?;
    Ok(())
}

fn reload() {
    let _ = std::process::Command::new("hyprctl")
        .arg("reload")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::validate_hotkey;

    #[test]
    fn accepts_ordinary_combinations() {
        assert_eq!(validate_hotkey(" ctrl + space ").unwrap(), "CTRL + SPACE");
        assert_eq!(validate_hotkey("SUPER + K").unwrap(), "SUPER + K");
    }

    #[test]
    fn rejects_anything_that_could_inject_lua() {
        for bad in ["\") os.execute(\"rm -rf /", "SUPER + \"", "a\nb", ""] {
            assert!(validate_hotkey(bad).is_err(), "accepted {bad:?}");
        }
    }
}

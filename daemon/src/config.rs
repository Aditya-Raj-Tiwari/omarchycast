//! User settings, stored as JSON so the QML settings panel can round-trip them
//! without a schema translation layer. Every field has a default, so a config
//! written by an older version still loads.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_HOTKEY: &str = "CTRL + SPACE";

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("omacast/config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Providers {
    pub apps: bool,
    pub calculator: bool,
    pub dates: bool,
    pub notes: bool,
    /// Cap per provider, applied before results are merged, so one chatty
    /// provider can't crowd out the others.
    pub apps_limit: usize,
    pub notes_limit: usize,
    /// Where markdown notes live. Empty means the default, `~/Notes`.
    pub notes_directory: String,
}

impl Default for Providers {
    fn default() -> Self {
        Providers {
            apps: true,
            calculator: true,
            dates: true,
            notes: true,
            apps_limit: 20,
            notes_limit: 8,
            notes_directory: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Appearance {
    pub width: u32,
    pub rows_visible: u32,
    pub corner_radius: u32,
    /// When false the overlay uses its own palette instead of the Omarchy theme.
    pub follow_theme: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { width: 720, rows_visible: 8, corner_radius: 16, follow_theme: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Behaviour {
    pub hide_on_blur: bool,
    /// Escape clears a non-empty query before it dismisses the launcher.
    pub esc_clears_first: bool,
    pub show_recent_when_empty: bool,
    /// Cleared once the first-run tour has been shown, so it appears exactly once.
    pub tour_seen: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Behaviour {
            hide_on_blur: true,
            esc_clears_first: true,
            show_recent_when_empty: true,
            tour_seen: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub hotkey: String,
    pub providers: Providers,
    pub appearance: Appearance,
    pub behaviour: Behaviour,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: DEFAULT_HOTKEY.to_string(),
            providers: Providers::default(),
            appearance: Appearance::default(),
            behaviour: Behaviour::default(),
        }
    }
}

impl Config {
    /// Resolves the configured notes directory, falling back to the default when
    /// the setting is blank.
    pub fn notes_directory(&self) -> std::path::PathBuf {
        let configured = self.providers.notes_directory.trim();
        if configured.is_empty() {
            crate::providers::notes::default_directory()
        } else {
            std::path::PathBuf::from(shellexpand_home(configured))
        }
    }

    pub fn load() -> Config {
        let mut config: Config = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if config.hotkey.trim().is_empty() {
            config.hotkey = DEFAULT_HOTKEY.to_string();
        }
        config
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn provider_enabled(&self, id: &str) -> bool {
        match id {
            "apps" => self.providers.apps,
            "calc" => self.providers.calculator,
            "date" => self.providers.dates,
            "note" => self.providers.notes,
            _ => true,
        }
    }

    pub fn provider_limit(&self, id: &str) -> usize {
        match id {
            "apps" => self.providers.apps_limit.max(1),
            "note" => self.providers.notes_limit.max(1),
            // Calculator and dates emit a single pinned row.
            _ => 4,
        }
    }
}

/// Expands a leading `~` so the setting can be typed the way people write paths.
fn shellexpand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        None => path.to_string(),
    }
}

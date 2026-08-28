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
        .join("omarchycast/config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Providers {
    pub apps: bool,
    pub calculator: bool,
    pub dates: bool,
    pub notes: bool,
    pub plugins: bool,
    pub omarchy: bool,
    /// Cap per provider, applied before results are merged, so one chatty
    /// provider can't crowd out the others.
    pub apps_limit: usize,
    pub notes_limit: usize,
    pub plugins_limit: usize,
    pub omarchy_limit: usize,
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
            plugins: true,
            omarchy: true,
            apps_limit: 20,
            notes_limit: 8,
            plugins_limit: 10,
            omarchy_limit: 8,
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
    /// Tighter rows and paddings; the default is the comfortable layout.
    pub compact: bool,
    /// Percentage applied to every font size the theme provides.
    pub font_scale: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            width: 720,
            rows_visible: 8,
            corner_radius: 16,
            follow_theme: true,
            compact: false,
            font_scale: 100,
        }
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
        let mut config: Config =
            crate::safeio::read_capped_optional(&config_path(), crate::limits::MAX_CONFIG_BYTES)
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        config.sanitise();
        config
    }

    pub fn save(&self) -> Result<()> {
        crate::safeio::write_atomic(&config_path(), &serde_json::to_string_pretty(self)?)
    }

    /// Clamps every setting into its legal range. Runs on load and on every
    /// config received over IPC, so no field is ever trusted merely because it
    /// deserialised: a hand-edited file and a socket message get the same rules.
    pub fn sanitise(&mut self) {
        use crate::limits::*;
        if self.hotkey.trim().is_empty() || self.hotkey.chars().count() > MAX_HOTKEY_CHARS {
            self.hotkey = DEFAULT_HOTKEY.to_string();
        }
        self.providers.apps_limit = self.providers.apps_limit.clamp(1, MAX_PROVIDER_RESULTS);
        self.providers.notes_limit = self.providers.notes_limit.clamp(1, MAX_PROVIDER_RESULTS);
        self.providers.plugins_limit = self.providers.plugins_limit.clamp(1, MAX_PROVIDER_RESULTS);
        self.providers.omarchy_limit = self.providers.omarchy_limit.clamp(1, MAX_PROVIDER_RESULTS);
        if self.providers.notes_directory.chars().count() > MAX_PATH_SETTING_CHARS {
            self.providers.notes_directory = String::new();
        }
        self.appearance.width = self.appearance.width.clamp(320, 1600);
        self.appearance.rows_visible = self.appearance.rows_visible.clamp(3, 20);
        self.appearance.corner_radius = self.appearance.corner_radius.min(48);
        self.appearance.font_scale = self.appearance.font_scale.clamp(70, 160);
    }

    pub fn provider_enabled(&self, id: &str) -> bool {
        match id {
            "apps" => self.providers.apps,
            "calc" => self.providers.calculator,
            "date" => self.providers.dates,
            "note" => self.providers.notes,
            "plug" => self.providers.plugins,
            "oma" => self.providers.omarchy,
            _ => true,
        }
    }

    pub fn provider_limit(&self, id: &str) -> usize {
        match id {
            "apps" => self.providers.apps_limit.max(1),
            "note" => self.providers.notes_limit.max(1),
            "plug" => self.providers.plugins_limit.max(1),
            "oma" => self.providers.omarchy_limit.max(1),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_clamps_every_field_into_range() {
        let mut c = Config {
            hotkey: "K".repeat(500),
            ..Config::default()
        };
        c.providers.apps_limit = 9999;
        c.providers.notes_limit = 0;
        c.appearance.width = 1;
        c.appearance.rows_visible = 999;
        c.appearance.corner_radius = 10_000;
        c.sanitise();
        assert_eq!(c.hotkey, DEFAULT_HOTKEY);
        assert_eq!(c.providers.apps_limit, crate::limits::MAX_PROVIDER_RESULTS);
        assert_eq!(c.providers.notes_limit, 1);
        assert_eq!(c.appearance.width, 320);
        assert_eq!(c.appearance.rows_visible, 20);
        assert_eq!(c.appearance.corner_radius, 48);
    }
}

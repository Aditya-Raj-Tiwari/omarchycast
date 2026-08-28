use crate::core::score::{combine, frecency_boost, pattern, score_fields};
use crate::core::store::{now_unix, Store};
use crate::core::{Action, Item, Provider, Query};
use anyhow::{anyhow, Result};
use freedesktop_desktop_entry::{current_desktop, desktop_entries, get_languages_from_env};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const PROVIDER: &str = "apps";

struct AppEntry {
    /// Desktop file id, e.g. `firefox.desktop` — also the frecency key.
    id: String,
    path: PathBuf,
    name: String,
    subtitle: Option<String>,
    icon: Option<String>,
    /// Secondary match fields: generic name, keywords, executable basename.
    aliases: Vec<String>,
}

pub struct AppsProvider {
    entries: RwLock<Vec<AppEntry>>,
    store: Arc<Store>,
}

impl AppsProvider {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        let provider = Arc::new(AppsProvider { entries: RwLock::new(Vec::new()), store });
        provider.reindex();
        provider
    }

    pub fn watch_paths() -> Vec<PathBuf> {
        freedesktop_desktop_entry::default_paths().collect()
    }
}

/// The icon theme Omarchy's current theme asks for, falling back to the GTK setting
/// and finally to hicolor, which every compliant icon set inherits from.
fn icon_theme() -> String {
    let omarchy = dirs::home_dir()
        .map(|h| h.join(".local/state/omarchy/current/theme/icons.theme"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    omarchy
        .or_else(freedesktop_icons::default_theme_gtk)
        .unwrap_or_else(|| "hicolor".to_string())
}

fn resolve_icon(name: &str, theme: &str) -> Option<String> {
    // Desktop entries are allowed to give an absolute path instead of a theme name.
    if name.starts_with('/') {
        let p = Path::new(name);
        return p.exists().then(|| name.to_string());
    }
    freedesktop_icons::lookup(name)
        .with_size(64)
        .with_theme(theme)
        .with_cache()
        .find()
        .and_then(|p| p.to_str().map(str::to_string))
}

/// Strip the `%f`/`%U`/… field codes so the Exec line yields a usable binary name.
fn exec_basename(exec: &str) -> Option<String> {
    exec.split_whitespace()
        .find(|tok| !tok.starts_with('%') && !tok.contains('='))
        .and_then(|tok| Path::new(tok).file_name())
        .and_then(|s| s.to_str())
        .map(str::to_string)
}

fn index() -> Vec<AppEntry> {
    let locales = get_languages_from_env();
    let theme = icon_theme();
    let desktop = current_desktop().unwrap_or_default();

    let mut out: Vec<AppEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in desktop_entries(&locales) {
        if entry.no_display() || entry.hidden() || entry.type_() != Some("Application") {
            continue;
        }
        // Honour OnlyShowIn/NotShowIn so we don't offer GNOME-only control panels.
        if let Some(only) = entry.only_show_in() {
            if !only.iter().any(|d| desktop.iter().any(|c| c.eq_ignore_ascii_case(d))) {
                continue;
            }
        }
        if let Some(not) = entry.not_show_in() {
            if not.iter().any(|d| desktop.iter().any(|c| c.eq_ignore_ascii_case(d))) {
                continue;
            }
        }
        // TryExec names a binary that must exist for the entry to be valid.
        if let Some(try_exec) = entry.try_exec() {
            if try_exec.starts_with('/') && !Path::new(try_exec).exists() {
                continue;
            }
        }
        if entry.exec().is_none() {
            continue;
        }

        let id = entry
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&entry.appid)
            .to_string();
        // Earlier paths win: ~/.local/share overrides /usr/share for the same id.
        if !seen.insert(id.clone()) {
            continue;
        }

        let name = entry
            .name(&locales)
            .map(|c| c.to_string())
            .unwrap_or_else(|| entry.appid.clone());

        let mut aliases = Vec::new();
        if let Some(g) = entry.generic_name(&locales) {
            aliases.push(g.to_string());
        }
        if let Some(kw) = entry.keywords(&locales) {
            aliases.push(kw.iter().map(|k| k.as_ref()).collect::<Vec<_>>().join(" "));
        }
        if let Some(bin) = entry.exec().and_then(exec_basename) {
            aliases.push(bin);
        }
        aliases.retain(|a| !a.is_empty());

        let subtitle = entry
            .comment(&locales)
            .map(|c| c.to_string())
            .or_else(|| entry.generic_name(&locales).map(|g| g.to_string()));

        let icon = entry.icon().and_then(|i| resolve_icon(i, &theme));

        out.push(AppEntry { id, path: entry.path.clone(), name, subtitle, icon, aliases });
    }
    out
}

impl Provider for AppsProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        let entries = match self.entries.read() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let now = now_unix();
        let pat = (!q.is_empty()).then(|| pattern(&q.trimmed));

        entries
            .iter()
            .filter_map(|entry| {
                let usage = self.store.usage(&entry.id);
                let boost = frecency_boost(usage.launches, usage.last_used, now);

                let score = match &pat {
                    // Empty query: the list is pure frecency, then alphabetical.
                    None => boost,
                    Some(pat) => {
                        let aliases: Vec<&str> = entry.aliases.iter().map(String::as_str).collect();
                        combine(score_fields(pat, &entry.name, &aliases)?, boost)
                    }
                };

                let mut item = Item::new(PROVIDER, "Application", entry.id.clone(), entry.name.clone());
                item.subtitle = entry.subtitle.clone();
                item.icon = entry.icon.clone();
                item.glyph = Some("\u{25a2}".to_string());
                item.score = score;
                Some(item)
            })
            .collect()
    }

    fn activate(&self, id: &str, _action: Action) -> Result<()> {
        let path = {
            let entries = self.entries.read().map_err(|_| anyhow!("apps index poisoned"))?;
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.path.clone())
                .ok_or_else(|| anyhow!("unknown app: {id}"))?
        };
        // `gio launch` handles Exec field codes, Terminal= and DBusActivatable=.
        crate::launch::detached("gio", &[std::ffi::OsStr::new("launch"), path.as_os_str()])?;
        self.store.record_launch(id);
        Ok(())
    }

    fn reindex(&self) {
        let fresh = index();
        if let Ok(mut entries) = self.entries.write() {
            *entries = fresh;
        }
    }
}

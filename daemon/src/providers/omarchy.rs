//! Omarchy itself, searchable: every entry of the Omarchy menu, every
//! documented `omarchy` command that can run without arguments, and one row
//! per installed theme.
//!
//! Sources, all read at index time and on explicit reindex:
//!   * `omarchy commands --json` — the CLI's own machine-readable listing
//!   * the menu definition (`omarchy-menu.jsonc`, user copy first)
//!   * the theme directories
//!
//! Menu actions are simple command strings by construction; anything that
//! smells of shell syntax is not executed directly — activation falls back to
//! summoning the menu at that entry, which is the surface built to run it.

use crate::core::score::{combine, pattern, score_fields};
use crate::core::{Action, Item, Provider, Query};
use crate::limits;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const PROVIDER: &str = "oma";
/// Index-time budget for `omarchy commands --json`; generous, never per-keystroke.
const INDEX_BUDGET: Duration = Duration::from_secs(5);
const MAX_INDEX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 1500;

#[derive(Debug, Clone)]
enum Run {
    /// Argv executed detached — menu actions and bare `omarchy` routes.
    Argv(Vec<String>),
    /// Open the Omarchy menu at this id; used when an action isn't plain argv.
    Summon(String),
}

struct Entry {
    title: String,
    subtitle: String,
    keywords: String,
    glyph: &'static str,
    run: Run,
}

pub struct OmarchyProvider {
    entries: RwLock<Vec<Entry>>,
}

/// Bounded run of a trusted indexing command: wall-clock budget, output cap.
fn bounded_index_exec(argv: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut capped = stdout.take(MAX_INDEX_OUTPUT as u64 + 1);
        let _ = capped.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + INDEX_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("indexing command timed out");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                return Err(e.into());
            }
        }
    }
    let buffer = reader.join().map_err(|_| anyhow!("reader panicked"))?;
    if buffer.len() > MAX_INDEX_OUTPUT {
        bail!("indexing output too large");
    }
    Ok(buffer)
}

// ---------------------------------------------------------------- commands

#[derive(Debug, Deserialize)]
struct CommandsFile {
    #[serde(default)]
    commands: Vec<CommandRow>,
}

#[derive(Debug, Deserialize)]
struct CommandRow {
    route: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    args: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    requires_sudo: bool,
}

/// Only commands whose whole signature is optional can be run from a launcher
/// row — `[--status]` yes, `<pid>` no.
fn runnable_bare(args: &str) -> bool {
    !args.contains('<')
}

fn command_entries() -> Vec<Entry> {
    let Ok(raw) = bounded_index_exec(&["omarchy", "commands", "--json"]) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<CommandsFile>(&raw) else {
        return Vec::new();
    };
    parsed
        .commands
        .into_iter()
        .filter(|c| !c.hidden && !c.requires_sudo && runnable_bare(&c.args))
        .map(|c| {
            let argv: Vec<String> = c.route.split_whitespace().map(str::to_string).collect();
            Entry {
                keywords: format!("{} {}", c.aliases.join(" "), c.summary),
                title: c.route,
                subtitle: c.summary,
                glyph: "\u{f303}", // Arch glyph; falls back fine in any nerd font
                run: Run::Argv(argv),
            }
        })
        .collect()
}

// -------------------------------------------------------------------- menu

/// Strips `//` and `/* */` comments outside strings, then trailing commas, so
/// serde_json can parse the menu's JSONC. String-aware: a URL inside an
/// action survives.
fn jsonc_to_json(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Trailing commas: `, }` and `, ]` are legal JSONC but not JSON.
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        if c == ',' {
            let mut j = idx + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                continue;
            }
        }
        cleaned.push(c);
    }
    cleaned
}

#[derive(Debug, Deserialize)]
struct MenuItem {
    #[serde(default)]
    label: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
}

/// Default first, then the user's extension file, which overlays it — the
/// extensions directory adds entries, it does not replace the menu.
fn menu_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let default = PathBuf::from(
        std::env::var_os("OMARCHY_PATH").unwrap_or_else(|| "/usr/share/omarchy".into()),
    )
    .join("default/omarchy/omarchy-menu.jsonc");
    if default.is_file() {
        files.push(default);
    }
    if let Some(config) = dirs::config_dir() {
        let user = config.join("omarchy/extensions/omarchy-menu.jsonc");
        if user.is_file() {
            files.push(user);
        }
    }
    files
}

/// True when an action is a plain space-separated command we can run as argv.
/// Anything with shell syntax is left to the menu itself.
fn plain_argv(action: &str) -> Option<Vec<String>> {
    const SHELL_CHARS: &[char] =
        &['|', '&', ';', '<', '>', '(', ')', '$', '`', '"', '\'', '\\', '*', '?', '{', '~', '\n'];
    if action.contains(SHELL_CHARS) {
        return None;
    }
    let argv: Vec<String> = action.split_whitespace().map(str::to_string).collect();
    (!argv.is_empty()).then_some(argv)
}

fn menu_entries() -> Vec<Entry> {
    let mut map: BTreeMap<String, MenuItem> = BTreeMap::new();
    for path in menu_files() {
        // The default menu is packaged in /usr/share and root-owned, so the
        // owner check does not apply to it; the user overlay gets the full
        // safeio treatment. Both are size-capped.
        let raw = if path.starts_with("/usr/share") {
            std::fs::read_to_string(&path).ok()
        } else {
            crate::safeio::read_capped_optional(&path, limits::MAX_HYPR_CONFIG_BYTES)
        };
        let Some(raw) = raw else { continue };
        if raw.len() as u64 > limits::MAX_HYPR_CONFIG_BYTES {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<BTreeMap<String, MenuItem>>(&jsonc_to_json(&raw))
        {
            // Later files override earlier ids, matching the menu's own overlay rule.
            map.extend(parsed);
        }
    }
    if map.is_empty() {
        return Vec::new();
    }

    // id -> label, for breadcrumb subtitles like "System › Suspend".
    let labels: BTreeMap<&str, &str> =
        map.iter().map(|(id, item)| (id.as_str(), item.label.as_str())).collect();
    let breadcrumb = |id: &str| -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut prefix = String::new();
        for segment in id.split('.') {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(segment);
            if let Some(label) = labels.get(prefix.as_str()) {
                parts.push(label);
            }
        }
        parts.join(" \u{203a} ")
    };

    map.iter()
        .filter(|(_, item)| !item.label.is_empty())
        .map(|(id, item)| {
            let run = match (&item.action, &item.target) {
                (Some(action), _) => plain_argv(action)
                    .map(Run::Argv)
                    .unwrap_or_else(|| Run::Summon(id.clone())),
                (None, Some(target)) => {
                    Run::Argv(vec!["xdg-open".to_string(), target.clone()])
                }
                (None, None) => Run::Summon(id.clone()),
            };
            let is_submenu = matches!(run, Run::Summon(_)) && item.action.is_none();
            Entry {
                title: item.label.clone(),
                subtitle: if is_submenu {
                    format!("Omarchy menu \u{203a} {}", breadcrumb(id))
                } else {
                    breadcrumb(id)
                },
                keywords: format!("{} {}", item.aliases.join(" "), id.replace('.', " ")),
                glyph: "\u{eb6c}",
                run,
            }
        })
        .collect()
}

// ------------------------------------------------------------------- themes

fn theme_entries() -> Vec<Entry> {
    let mut names: Vec<String> = Vec::new();
    let mut dirs_to_scan: Vec<PathBuf> = Vec::new();
    if let Some(config) = dirs::config_dir() {
        dirs_to_scan.push(config.join("omarchy/themes"));
    }
    dirs_to_scan.push(
        PathBuf::from(
            std::env::var_os("OMARCHY_PATH").unwrap_or_else(|| "/usr/share/omarchy".into()),
        )
        .join("themes"),
    );
    for dir in dirs_to_scan {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| Entry {
            title: format!("Theme: {name}"),
            subtitle: "Switch the Omarchy theme".to_string(),
            keywords: "theme style appearance".to_string(),
            glyph: "\u{e22b}",
            run: Run::Argv(vec![
                "omarchy".to_string(),
                "theme".to_string(),
                "set".to_string(),
                name,
            ]),
        })
        .collect()
}

// ----------------------------------------------------------------- provider

impl OmarchyProvider {
    pub fn new() -> Arc<Self> {
        let provider = Arc::new(OmarchyProvider { entries: RwLock::new(Vec::new()) });
        // The commands listing shells out; index off-thread so daemon startup
        // isn't gated on it.
        let background = provider.clone();
        std::thread::spawn(move || background.reindex());
        provider
    }
}

impl Provider for OmarchyProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        if q.is_empty() {
            return Vec::new();
        }
        let entries = match self.entries.read() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let pat = pattern(&q.trimmed);
        entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let relevance =
                    score_fields(&pat, &entry.title, &[entry.subtitle.as_str(), entry.keywords.as_str()])?;
                let mut item =
                    Item::new(PROVIDER, "Omarchy", index.to_string(), entry.title.clone());
                item.subtitle = Some(entry.subtitle.clone());
                item.glyph = Some(entry.glyph.to_string());
                item.score = combine(relevance, 0);
                Some(item)
            })
            .collect()
    }

    fn activate(&self, id: &str, _action: Action) -> Result<()> {
        let index: usize = id.parse()?;
        let entries = self.entries.read().map_err(|_| anyhow!("omarchy index poisoned"))?;
        let entry = entries.get(index).ok_or_else(|| anyhow!("stale omarchy entry"))?;
        match &entry.run {
            Run::Argv(argv) => {
                let args: Vec<&std::ffi::OsStr> =
                    argv[1..].iter().map(std::ffi::OsStr::new).collect();
                crate::launch::detached(&argv[0], &args)
            }
            Run::Summon(menu_id) => crate::launch::detached(
                "omarchy-menu",
                &[std::ffi::OsStr::new("summon"), std::ffi::OsStr::new(menu_id)],
            ),
        }
    }

    fn reindex(&self) {
        let mut fresh = menu_entries();
        fresh.extend(theme_entries());
        fresh.extend(command_entries());
        fresh.truncate(MAX_ENTRIES);
        if let Ok(mut entries) = self.entries.write() {
            *entries = fresh;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_stripper_preserves_urls_and_removes_comments() {
        let raw = r#"{
  // a comment
  "a": {"label": "L", "target": "https://x.y/z"}, /* block */
  "b": {"label": "M",},
}"#;
        let parsed: BTreeMap<String, MenuItem> =
            serde_json::from_str(&jsonc_to_json(raw)).expect("should parse");
        assert_eq!(parsed["a"].target.as_deref(), Some("https://x.y/z"));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn shell_syntax_is_never_run_directly() {
        assert!(plain_argv("omarchy-launch-screensaver force").is_some());
        for bad in ["a | b", "a && b", "a $(x)", "sh -c 'x'", "a > /tmp/f"] {
            assert!(plain_argv(bad).is_none(), "{bad} must fall back to summon");
        }
    }

    #[test]
    fn only_all_optional_signatures_run_bare() {
        assert!(runnable_bare(""));
        assert!(runnable_bare("[--status]"));
        assert!(!runnable_bare("<pid> [comm]"));
    }
}

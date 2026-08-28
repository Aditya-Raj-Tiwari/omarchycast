//! User plugins: drop a JSON manifest into `~/.config/omarchycast/plugins/` and
//! its commands join the search results — the same way notes did, but without
//! recompiling the daemon.
//!
//! A manifest contributes two things:
//!
//!   * `commands` — static entries, fuzzy-matched like applications. Activation
//!     either runs an argv (never a shell string) or copies text.
//!   * `query`    — an optional argv run when the query starts with the
//!     plugin's `keyword`. It receives the rest of the query as its final
//!     argument and prints one JSON object per line:
//!     `{"title": "...", "subtitle": "...", "copy": "..."}` or
//!     `{"title": "...", "exec": ["cmd", "arg"]}`
//!
//! Everything a plugin supplies is treated as untrusted input: manifests are
//! read through the capped, symlink-refusing path, script output is read
//! through a byte cap and a wall-clock budget, and every emitted field goes
//! through the same clamps as the built-in providers. What a plugin's own
//! programs do when activated is, deliberately, the user's business — they
//! installed it.

use crate::core::score::{combine, pattern, score_fields};
use crate::core::{Action, Item, Provider, Query};
use crate::limits;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const PROVIDER: &str = "plug";
pub const MAX_PLUGINS: usize = 64;
const MAX_COMMANDS_PER_PLUGIN: usize = 128;
const MAX_ARGV_ITEMS: usize = 64;
const MAX_ARG_BYTES: usize = 4096;
/// A dynamic query script gets this long to answer before it is killed.
const QUERY_BUDGET: Duration = Duration::from_millis(700);
/// And may print at most this much; the rest is discarded with the process.
const MAX_SCRIPT_OUTPUT: usize = 64 * 1024;
const MAX_DYNAMIC_ITEMS: usize = 32;

pub fn plugins_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("omarchycast/plugins")
}

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    query: Vec<String>,
    #[serde(default)]
    commands: Vec<ManifestCommand>,
}

#[derive(Debug, Deserialize)]
struct ManifestCommand {
    title: String,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    exec: Vec<String>,
    #[serde(default)]
    copy: Option<String>,
    #[serde(default)]
    glyph: Option<String>,
}

/// One action a row can perform. Argv only — a plugin never hands us a shell
/// string to interpret.
#[derive(Debug, Clone)]
enum Invocation {
    Exec(Vec<String>),
    Copy(String),
}

struct StaticCommand {
    plugin: String,
    title: String,
    subtitle: Option<String>,
    glyph: Option<String>,
    invocation: Invocation,
}

struct DynamicSource {
    plugin: String,
    keyword: String,
    argv: Vec<String>,
}

/// Result of the most recent dynamic query, kept so activation refers to what
/// is actually on screen instead of re-running the script.
struct DynamicResults {
    items: Vec<Invocation>,
}

pub struct PluginsProvider {
    commands: RwLock<Vec<StaticCommand>>,
    sources: RwLock<Vec<DynamicSource>>,
    last_dynamic: Mutex<DynamicResults>,
}

fn valid_argv(argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        bail!("empty command");
    }
    if argv.len() > MAX_ARGV_ITEMS {
        bail!("too many arguments");
    }
    if argv.iter().any(|a| a.len() > MAX_ARG_BYTES) {
        bail!("argument too long");
    }
    Ok(())
}

fn load_manifests() -> (Vec<StaticCommand>, Vec<DynamicSource>) {
    let mut commands = Vec::new();
    let mut sources = Vec::new();

    let Ok(entries) = std::fs::read_dir(plugins_dir()) else {
        return (commands, sources);
    };
    let mut manifest_paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    manifest_paths.sort();
    manifest_paths.truncate(MAX_PLUGINS);

    for path in manifest_paths {
        let Some(raw) = crate::safeio::read_capped_optional(&path, limits::MAX_CONFIG_BYTES) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Manifest>(&raw) else {
            eprintln!("omarchycastd: ignoring malformed plugin manifest {}", path.display());
            continue;
        };
        let plugin = limits::clamp_text(&manifest.name, 48);
        if plugin.is_empty() {
            continue;
        }

        for command in manifest.commands.into_iter().take(MAX_COMMANDS_PER_PLUGIN) {
            let invocation = match (&command.exec[..], command.copy) {
                ([], Some(text)) => Invocation::Copy(limits::clamp_text(&text, 8192)),
                (argv, None) if valid_argv(argv).is_ok() => Invocation::Exec(command.exec),
                // A command that is both, or neither, or invalid is skipped —
                // one bad entry must not take the rest of the plugin with it.
                _ => continue,
            };
            commands.push(StaticCommand {
                plugin: plugin.clone(),
                title: limits::clamp_text(&command.title, limits::MAX_TITLE_CHARS),
                subtitle: command.subtitle,
                glyph: command.glyph,
                invocation,
            });
        }

        if let Some(keyword) = manifest.keyword {
            let keyword = limits::clamp_text(&keyword, 24).to_lowercase();
            if !keyword.is_empty() && valid_argv(&manifest.query).is_ok() {
                sources.push(DynamicSource { plugin, keyword, argv: manifest.query });
            }
        }
    }
    (commands, sources)
}

/// Runs a plugin's query argv with the query text as the final argument,
/// bounded in both time and output. On overrun the process is killed.
fn run_query_script(argv: &[String], needle: &str) -> Result<Vec<u8>> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(needle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("could not run {}: {e}", argv[0]))?;

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let deadline = Instant::now() + QUERY_BUDGET;

    // Reader thread so the time budget holds even if the script never writes.
    let handle = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut capped = stdout.take(MAX_SCRIPT_OUTPUT as u64 + 1);
        let _ = capped.read_to_end(&mut buffer);
        buffer
    });

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("plugin query exceeded {}ms", QUERY_BUDGET.as_millis());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                return Err(e.into());
            }
        }
    }

    let buffer = handle.join().map_err(|_| anyhow!("output reader panicked"))?;
    if buffer.len() > MAX_SCRIPT_OUTPUT {
        bail!("plugin query printed more than {MAX_SCRIPT_OUTPUT} bytes");
    }
    Ok(buffer)
}

#[derive(Debug, Deserialize)]
struct ScriptItem {
    title: String,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    accessory: Option<String>,
    #[serde(default)]
    exec: Vec<String>,
    #[serde(default)]
    copy: Option<String>,
}

impl PluginsProvider {
    pub fn new() -> Arc<Self> {
        let provider = Arc::new(PluginsProvider {
            commands: RwLock::new(Vec::new()),
            sources: RwLock::new(Vec::new()),
            last_dynamic: Mutex::new(DynamicResults { items: Vec::new() }),
        });
        provider.reindex();
        provider
    }

    fn dynamic_items(&self, source: &DynamicSource, needle: &str) -> Vec<Item> {
        let Ok(output) = run_query_script(&source.argv, needle) else {
            return Vec::new();
        };
        let plugin_name = source.plugin.clone();
        let Ok(text) = String::from_utf8(output) else { return Vec::new() };

        let mut invocations = Vec::new();
        let mut items = Vec::new();
        for line in text.lines().take(MAX_DYNAMIC_ITEMS) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<ScriptItem>(line) else { continue };
            let invocation = match (&parsed.exec[..], parsed.copy) {
                ([], Some(copy)) => Invocation::Copy(limits::clamp_text(&copy, 8192)),
                (argv, None) if valid_argv(argv).is_ok() => Invocation::Exec(parsed.exec),
                _ => continue,
            };

            let index = invocations.len();
            invocations.push(invocation);

            let mut item = Item::new(
                PROVIDER,
                "Plugin",
                format!("dyn:{index}"),
                parsed.title,
            );
            item.subtitle = parsed.subtitle.or_else(|| Some(plugin_name.clone()));
            item.accessory = parsed.accessory;
            item.glyph = Some("\u{2699}".to_string());
            // Keyword-scoped results are what the user explicitly asked for;
            // rank them like calculator answers, in printed order.
            item.score = 900_000 - index as i64;
            items.push(item);
        }

        if let Ok(mut last) = self.last_dynamic.lock() {
            last.items = invocations;
        }
        items
    }

    fn activate_invocation(invocation: &Invocation) -> Result<()> {
        match invocation {
            Invocation::Exec(argv) => {
                valid_argv(argv)?;
                let args: Vec<&std::ffi::OsStr> =
                    argv[1..].iter().map(std::ffi::OsStr::new).collect();
                crate::launch::detached(&argv[0], &args)
            }
            Invocation::Copy(text) => crate::clipboard::copy_text(text),
        }
    }
}

impl Provider for PluginsProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        if q.is_empty() {
            return Vec::new();
        }

        // Keyword-scoped dynamic sources run only when explicitly addressed,
        // so ordinary typing never spawns a process.
        if let Ok(sources) = self.sources.read() {
            for source in sources.iter() {
                if let Some(rest) = q.trimmed.strip_prefix(&source.keyword) {
                    if let Some(needle) = rest.strip_prefix(' ') {
                        return self.dynamic_items(source, needle.trim());
                    }
                }
            }
        }

        let commands = match self.commands.read() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let pat = pattern(&q.trimmed);
        commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                let relevance =
                    score_fields(&pat, &command.title, &[command.plugin.as_str()])?;
                let mut item = Item::new(
                    PROVIDER,
                    "Plugin",
                    format!("cmd:{index}"),
                    command.title.clone(),
                );
                item.subtitle =
                    command.subtitle.clone().or_else(|| Some(command.plugin.clone()));
                item.glyph = command.glyph.clone().or_else(|| Some("\u{2699}".to_string()));
                item.score = combine(relevance, 0);
                Some(item)
            })
            .collect()
    }

    fn activate(&self, id: &str, _action: Action) -> Result<()> {
        if let Some(index) = id.strip_prefix("cmd:") {
            let index: usize = index.parse()?;
            let commands = self.commands.read().map_err(|_| anyhow!("plugin index poisoned"))?;
            let command = commands.get(index).ok_or_else(|| anyhow!("unknown command"))?;
            return Self::activate_invocation(&command.invocation);
        }
        if let Some(index) = id.strip_prefix("dyn:") {
            let index: usize = index.parse()?;
            let last = self.last_dynamic.lock().map_err(|_| anyhow!("plugin state poisoned"))?;
            let invocation =
                last.items.get(index).ok_or_else(|| anyhow!("stale plugin result"))?.clone();
            drop(last);
            return Self::activate_invocation(&invocation);
        }
        Err(anyhow!("unknown plugin item: {id}"))
    }

    fn reindex(&self) {
        let (commands, sources) = load_manifests();
        if let Ok(mut c) = self.commands.write() {
            *c = commands;
        }
        if let Ok(mut s) = self.sources.write() {
            *s = sources;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_validation_rejects_the_dangerous_shapes() {
        assert!(valid_argv(&[]).is_err());
        assert!(valid_argv(&vec!["x".to_string(); MAX_ARGV_ITEMS + 1]).is_err());
        assert!(valid_argv(&["a".repeat(MAX_ARG_BYTES + 1)]).is_err());
        assert!(valid_argv(&["notify-send".to_string(), "hi".to_string()]).is_ok());
    }

    #[test]
    fn script_items_require_exactly_one_action() {
        let both = r#"{"title":"t","exec":["x"],"copy":"y"}"#;
        let neither = r#"{"title":"t"}"#;
        let copy = r#"{"title":"t","copy":"y"}"#;
        for (raw, ok) in [(both, false), (neither, false), (copy, true)] {
            let parsed: ScriptItem = serde_json::from_str(raw).unwrap();
            let valid = match (&parsed.exec[..], parsed.copy) {
                ([], Some(_)) => true,
                (argv, None) => valid_argv(argv).is_ok(),
                _ => false,
            };
            assert_eq!(valid, ok, "{raw}");
        }
    }

    #[test]
    fn query_scripts_are_killed_at_the_deadline() {
        let started = Instant::now();
        // The needle is passed as the final argument, so it doubles as sleep's
        // duration here: `sleep 10`.
        let result = run_query_script(&["sleep".to_string()], "10");
        assert!(result.is_err(), "a hung script must not hang the daemon");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn query_script_output_is_captured() {
        let out = run_query_script(&["echo".to_string(), "hello".to_string()], "ignored").unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("hello"));
    }
}

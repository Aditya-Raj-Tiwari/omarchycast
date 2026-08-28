pub mod score;
pub mod store;

use crate::config::Config;
use serde::Serialize;
use std::sync::Arc;

/// One row in the result list. `id` is namespaced by provider (`"apps:firefox.desktop"`)
/// so activation can be routed back without a second lookup table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub provider: &'static str,
    /// Human label for the right-hand pill: "Application", "Calculator", "Clipboard".
    pub kind: &'static str,
    pub title: String,
    pub subtitle: Option<String>,
    /// Absolute path to an icon file; the frontend runs it through `convertFileSrc`.
    pub icon: Option<String>,
    /// Fallback shown when `icon` is None.
    pub glyph: Option<String>,
    /// Right-aligned text, used by the calculator for the result.
    pub accessory: Option<String>,
    #[serde(skip)]
    pub score: i64,
}

impl Item {
    pub fn new(provider: &'static str, kind: &'static str, id: String, title: String) -> Self {
        Item {
            id: format!("{provider}:{id}"),
            provider,
            kind,
            title,
            subtitle: None,
            icon: None,
            glyph: None,
            accessory: None,
            score: 0,
        }
    }
}

pub struct Query {
    pub trimmed: String,
}

impl Query {
    pub fn new(raw: &str) -> Self {
        Query { trimmed: raw.trim().to_string() }
    }
    pub fn is_empty(&self) -> bool {
        self.trimmed.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Primary,
    Secondary,
}

impl Action {
    pub fn parse(s: &str) -> Action {
        match s {
            "secondary" => Action::Secondary,
            _ => Action::Primary,
        }
    }
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Runs on every keystroke, so it must stay in the sub-millisecond range.
    /// Anything expensive belongs in `reindex`, not here.
    fn query(&self, q: &Query) -> Vec<Item>;

    /// `id` has already had the `"<provider>:"` prefix stripped.
    fn activate(&self, id: &str, action: Action) -> anyhow::Result<()>;

    /// Called at startup and whenever the filesystem watcher fires.
    fn reindex(&self) {}
}

pub struct Registry {
    providers: Vec<Arc<dyn Provider>>,
}

impl Registry {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        Registry { providers }
    }

    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    pub fn query(&self, raw: &str, config: &Config, total: usize) -> Vec<Item> {
        let q = Query::new(raw);
        let mut items: Vec<Item> = Vec::new();

        for provider in &self.providers {
            if !config.provider_enabled(provider.id()) {
                continue;
            }
            // Cap each provider before merging, so one source with hundreds of
            // weak matches can't crowd the others out of the visible rows.
            let mut produced = provider.query(&q);
            produced.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
            produced.truncate(config.provider_limit(provider.id()));
            items.append(&mut produced);
        }

        // Descending score, with the title as a stable tie-break so equal-scoring
        // results don't shuffle between keystrokes.
        items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        items.truncate(total);
        items
    }

    pub fn activate(&self, id: &str, action: Action) -> anyhow::Result<()> {
        let (provider_id, rest) = id
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed item id: {id}"))?;
        let provider = self
            .providers
            .iter()
            .find(|p| p.id() == provider_id)
            .ok_or_else(|| anyhow::anyhow!("no provider named {provider_id}"))?;
        provider.activate(rest, action)
    }
}

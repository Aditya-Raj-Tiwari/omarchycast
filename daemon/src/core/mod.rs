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

impl Item {
    /// Enforces the display-field limits in `limits`. Runs once per emitted
    /// item, immediately before serialisation.
    pub fn clamp(&mut self) {
        use crate::limits::*;
        self.title = clamp_text(&self.title, MAX_TITLE_CHARS);
        self.subtitle = clamp_text_opt(self.subtitle.take(), MAX_SUBTITLE_CHARS);
        self.glyph = clamp_text_opt(self.glyph.take(), MAX_GLYPH_CHARS);
        self.accessory = clamp_text_opt(self.accessory.take(), MAX_ACCESSORY_CHARS);
        if self.icon.as_ref().is_some_and(|i| i.len() > MAX_ICON_PATH_BYTES) {
            self.icon = None;
        }
        self.id.truncate_to_char_boundary(MAX_ITEM_ID_BYTES);
    }
}

trait TruncateToBoundary {
    fn truncate_to_char_boundary(&mut self, max_bytes: usize);
}

impl TruncateToBoundary for String {
    fn truncate_to_char_boundary(&mut self, max_bytes: usize) {
        if self.len() <= max_bytes {
            return;
        }
        let mut end = max_bytes;
        while end > 0 && !self.is_char_boundary(end) {
            end -= 1;
        }
        self.truncate(end);
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
        let query_lower = q.trimmed.to_lowercase();
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
            // Two ordering corrections the raw fuzzy score cannot see. A title
            // the query is a prefix of should edge out a scattered match, and at
            // equal relevance a launchable application should sit above the rows
            // that merely mention the same name (install/remove menu entries).
            // Pinned rows (calculator, dates) stay above all of this.
            let priority: i64 = match provider.id() {
                "apps" => 400,
                "note" => 200,
                "plug" => 100,
                _ => 0,
            };
            for item in &mut produced {
                if item.score < 500_000 {
                    item.score += score::query_bonus(&query_lower, &item.title) + priority;
                }
                // Bound every field on the way out. Providers index files the
                // user does not control line by line, so the boundary treats
                // their output as data, not as trusted strings.
                item.clamp();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bounds_every_display_field() {
        let mut item = Item::new("apps", "Application", "x".repeat(5000), "T".repeat(5000));
        item.subtitle = Some("s".repeat(5000));
        item.accessory = Some("a".repeat(5000));
        item.glyph = Some("g".repeat(50));
        item.icon = Some("i".repeat(10_000));
        item.clamp();
        assert!(item.title.chars().count() <= crate::limits::MAX_TITLE_CHARS);
        assert!(item.subtitle.unwrap().chars().count() <= crate::limits::MAX_SUBTITLE_CHARS);
        assert!(item.accessory.unwrap().chars().count() <= crate::limits::MAX_ACCESSORY_CHARS);
        assert!(item.glyph.unwrap().chars().count() <= crate::limits::MAX_GLYPH_CHARS);
        assert!(item.icon.is_none(), "over-long icon path must be dropped");
        assert!(item.id.len() <= crate::limits::MAX_ITEM_ID_BYTES);
    }

    #[test]
    fn clamp_strips_control_sequences_from_titles() {
        let mut item = Item::new("apps", "Application", "id".into(), "evil\u{1b}[2Jtitle".into());
        item.clamp();
        assert_eq!(item.title, "evil[2Jtitle");
    }
}

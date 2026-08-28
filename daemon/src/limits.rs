//! Hard bounds for everything that crosses a trust boundary.
//!
//! The daemon reads from a socket, from configuration, and from files on disk,
//! and hands text to a QML consumer. Each of those is a place where "however big
//! it happens to be" turns into an allocation, so every one of them gets a
//! ceiling here rather than a scattered magic number.

/// A single request line. Anything longer is refused without being buffered.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// A serialised response. Larger means a provider misbehaved; send an error instead.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Simultaneous socket clients. The real consumer is a single overlay.
pub const MAX_CLIENTS: usize = 8;

pub const MAX_QUERY_CHARS: usize = 512;
pub const MAX_ITEM_ID_BYTES: usize = 1024;
pub const MAX_ACTION_BYTES: usize = 32;

/// Total rows returned, and the ceiling a per-provider setting may request.
pub const MAX_RESULTS: usize = 50;
pub const MAX_PROVIDER_RESULTS: usize = 40;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SUBTITLE_CHARS: usize = 300;
pub const MAX_ACCESSORY_CHARS: usize = 64;
pub const MAX_GLYPH_CHARS: usize = 8;
pub const MAX_ICON_PATH_BYTES: usize = 4096;
pub const MAX_ERROR_CHARS: usize = 300;

pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;
pub const MAX_NOTE_BYTES: u64 = 1024 * 1024;
pub const MAX_NOTES: usize = 2000;
pub const MAX_HYPR_CONFIG_BYTES: u64 = 1024 * 1024;

pub const MAX_HOTKEY_CHARS: usize = 64;
pub const MAX_PATH_SETTING_CHARS: usize = 4096;

/// Truncates on a character boundary and strips control characters, which have
/// no business in a search result and can misrepresent what a row says.
pub fn clamp_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

pub fn clamp_text_opt(value: Option<String>, max_chars: usize) -> Option<String> {
    value.map(|v| clamp_text(&v, max_chars)).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamping_respects_character_boundaries() {
        // Truncating by bytes would split these and produce invalid UTF-8.
        assert_eq!(clamp_text("ünïcödé", 3), "ünï");
        assert_eq!(clamp_text("日本語テキスト", 2), "日本");
    }

    #[test]
    fn control_characters_are_removed() {
        assert_eq!(clamp_text("a\u{0}b\u{1b}[31mc\nd", 64), "ab[31mcd");
        assert_eq!(clamp_text("\u{7}\u{7}", 64), "");
    }

    #[test]
    fn empty_after_clamping_becomes_none() {
        assert_eq!(clamp_text_opt(Some("\n\n".to_string()), 16), None);
        assert_eq!(clamp_text_opt(Some("ok".to_string()), 16), Some("ok".to_string()));
    }
}

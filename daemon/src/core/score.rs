use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::cell::RefCell;

thread_local! {
    // Matcher carries reusable scratch buffers and needs `&mut`, so it lives in a
    // thread-local rather than behind a lock on the hot path.
    static MATCHER: RefCell<Matcher> = RefCell::new(Matcher::new(Config::DEFAULT));
}

pub fn pattern(raw: &str) -> Pattern {
    Pattern::parse(raw, CaseMatching::Smart, Normalization::Smart)
}

pub fn score(pattern: &Pattern, haystack: &str) -> Option<u32> {
    MATCHER.with(|m| {
        let mut buf = Vec::new();
        pattern.score(Utf32Str::new(haystack, &mut buf), &mut m.borrow_mut())
    })
}

/// Best score across several fields, with a penalty for matching a secondary
/// field so a hit on the app's name always outranks a hit on its keywords.
pub fn score_fields(pattern: &Pattern, primary: &str, secondary: &[&str]) -> Option<u32> {
    let mut best = score(pattern, primary);
    for field in secondary {
        if let Some(s) = score(pattern, field) {
            let penalised = s.saturating_sub(s / 3);
            best = Some(best.map_or(penalised, |b| b.max(penalised)));
        }
    }
    best
}

const HALF_LIFE_DAYS: f64 = 14.0;

/// Frecency: usage count damped logarithmically, decayed by how long ago the last
/// use was. Returns roughly 0..800 so it can lift a result by a handful of fuzzy
/// points without ever overpowering a clearly better textual match.
pub fn frecency_boost(launches: u32, last_used_unix: i64, now_unix: i64) -> i64 {
    if launches == 0 {
        return 0;
    }
    let days_ago = ((now_unix - last_used_unix).max(0) as f64) / 86_400.0;
    let recency = 0.5f64.powf(days_ago / HALF_LIFE_DAYS);
    let volume = (1.0 + launches as f64).ln();
    (250.0 * volume * (0.3 + 0.7 * recency)) as i64
}

/// Fuzzy relevance dominates the ordering; frecency breaks ties and nudges
/// familiar results upward.
pub fn combine(fuzzy: u32, boost: i64) -> i64 {
    fuzzy as i64 * 100 + boost
}

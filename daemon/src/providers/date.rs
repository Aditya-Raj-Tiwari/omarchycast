//! Date arithmetic.
//!
//! This lives outside the calculator because fend-core 1.5's `set_current_time_v1`
//! is a no-op — its body is commented out and it forces `current_time = None` — so
//! the engine cannot answer anything involving "today". Rather than swap out a very
//! good units/maths engine, dates get their own small provider.

use crate::core::{Action, Item, Provider, Query};
use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const PROVIDER: &str = "date";
/// Same band as the calculator: an explicit date question outranks app matches.
const DATE_SCORE: i64 = 1_000_000;

const MONTHS: [&str; 12] = [
    "january", "february", "march", "april", "may", "june",
    "july", "august", "september", "october", "november", "december",
];
const WEEKDAYS: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// Days since the Unix epoch. All arithmetic happens in this one integer domain,
/// which sidesteps every timezone and leap-year trap except the one below.
type Day = i64;

/// Howard Hinnant's civil-date algorithm: proleptic Gregorian, valid far beyond
/// any range a launcher will see.
fn days_from_civil(y: i64, m: i64, d: i64) -> Day {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: Day) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Today in the user's local timezone, not UTC — "days until" is a question about
/// the calendar on the wall.
fn today() -> Day {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else { return 0 };
    let secs = now.as_secs() as i64;
    // SAFETY: `localtime_r` fills a `tm` we own from a `time_t` we own.
    let offset = unsafe {
        let time = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    };
    (secs + offset).div_euclid(86_400)
}

fn weekday(day: Day) -> &'static str {
    // 1970-01-01 was a Thursday, which is index 4 in a Sunday-first week.
    WEEKDAYS[(day + 4).rem_euclid(7) as usize]
}

fn format_date(day: Day) -> String {
    let (y, m, d) = civil_from_days(day);
    let month = MONTHS[(m - 1) as usize];
    let month = month[..1].to_uppercase() + &month[1..];
    format!("{}, {d} {month} {y}", weekday(day))
}

fn month_from_name(token: &str) -> Option<i64> {
    if token.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| *m == token || (token.len() >= 3 && m.starts_with(token)))
        .map(|i| i as i64 + 1)
}

/// Which way a year-less date should be resolved. "days until march 1" means the
/// next 1 March; "days since march 1" means the last one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefer {
    Future,
    Past,
}

/// Accepts `2026-10-08`, `october 8`, `8 october`, either with an optional year,
/// plus the three relative words people actually type.
fn parse_date(input: &str, today_day: Day, prefer: Prefer) -> Option<Day> {
    let text = input.trim();
    match text {
        "today" => return Some(today_day),
        "tomorrow" => return Some(today_day + 1),
        "yesterday" => return Some(today_day - 1),
        _ => {}
    }

    // ISO first: unambiguous, so it never has to guess a year.
    let iso: Vec<&str> = text.split('-').collect();
    if iso.len() == 3 {
        let y: i64 = iso[0].parse().ok()?;
        let m: i64 = iso[1].parse().ok()?;
        let d: i64 = iso[2].parse().ok()?;
        if (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m) {
            return Some(days_from_civil(y, m, d));
        }
        return None;
    }

    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() || tokens.len() > 3 {
        return None;
    }

    let mut month = None;
    let mut day = None;
    let mut year = None;
    for token in &tokens {
        if let Some(m) = month_from_name(token) {
            if month.replace(m).is_some() {
                return None;
            }
            continue;
        }
        // Ordinals are how dates get typed: "8th", "1st".
        let numeric = token.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let Ok(value) = numeric.parse::<i64>() else { return None };
        if numeric.len() == 4 || value > 31 {
            if year.replace(value).is_some() {
                return None;
            }
        } else if day.replace(value).is_some() {
            return None;
        }
    }

    let month = month?;
    let day = day?;
    let year = year.unwrap_or_else(|| {
        // Resolving a bare date towards the asked-for direction is what keeps
        // "days until"/"days since" from ever answering with a negative number.
        let (this_year, _, _) = civil_from_days(today_day);
        let this = days_from_civil(this_year, month, day);
        match prefer {
            Prefer::Future if this < today_day => this_year + 1,
            Prefer::Past if this > today_day => this_year - 1,
            _ => this_year,
        }
    });

    (day >= 1 && day <= days_in_month(year, month)).then(|| days_from_civil(year, month, day))
}

struct Answer {
    title: String,
    subtitle: String,
}

fn plural(n: i64, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

fn strip_any<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|p| text.strip_prefix(p)).map(str::trim)
}

fn answer(raw: &str) -> Option<Answer> {
    let text = raw.trim().to_lowercase();
    // "how many days until x" is the same question as "days until x".
    let text = text.strip_prefix("how many ").unwrap_or(&text).trim();
    let now = today();

    if let Some(rest) = strip_any(text, &["days until ", "days till ", "days to ", "days untill "]) {
        let target = parse_date(rest, now, Prefer::Future)?;
        let delta = target - now;
        return Some(Answer {
            title: plural(delta, "day"),
            subtitle: format!("until {}", format_date(target)),
        });
    }

    if let Some(rest) = strip_any(text, &["days since ", "days from "]) {
        let target = parse_date(rest, now, Prefer::Past)?;
        let delta = now - target;
        return Some(Answer {
            title: plural(delta, "day"),
            subtitle: format!("since {}", format_date(target)),
        });
    }

    // "<n> days from now" / "<n> weeks ago" and friends.
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() >= 3 {
        if let Ok(count) = tokens[0].parse::<i64>() {
            let span = match tokens[1].trim_end_matches('s') {
                "day" => 1,
                "week" => 7,
                "fortnight" => 14,
                _ => 0,
            };
            let tail = tokens[2..].join(" ");
            let direction = match tail.as_str() {
                "from now" | "from today" | "ahead" => 1,
                "ago" | "before now" | "before today" => -1,
                _ => 0,
            };
            if span > 0 && direction != 0 {
                let target = now + direction * count * span;
                return Some(Answer {
                    title: format_date(target),
                    subtitle: format!("{} {}", plural(count * span, "day"), if direction > 0 { "from today" } else { "ago" }),
                });
            }
        }
    }

    None
}

/// Exposed for the `omarchycast eval` debug subcommand.
pub fn eval_once(input: &str) -> Option<String> {
    answer(input).map(|a| format!("{} ({})", a.title, a.subtitle))
}

pub struct DateProvider {
    /// Remembered so activation copies exactly what was on screen.
    last: Mutex<Option<String>>,
}

impl DateProvider {
    pub fn new() -> Arc<Self> {
        Arc::new(DateProvider { last: Mutex::new(None) })
    }
}

impl Provider for DateProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        // Every supported phrasing contains one of these, so the common case costs
        // two substring scans rather than a parse.
        let lower = q.trimmed.to_lowercase();
        if !["day", "week", "fortnight"].iter().any(|k| lower.contains(k)) {
            return Vec::new();
        }
        let Some(answer) = answer(&q.trimmed) else { return Vec::new() };

        if let Ok(mut last) = self.last.lock() {
            *last = Some(answer.title.clone());
        }

        let mut item = Item::new(PROVIDER, "Date", "answer".to_string(), answer.title);
        item.subtitle = Some(answer.subtitle);
        item.glyph = Some("\u{1f5d3}".to_string());
        item.accessory = Some("↵ copy".to_string());
        item.score = DATE_SCORE;
        vec![item]
    }

    fn activate(&self, _id: &str, _action: Action) -> Result<()> {
        let value = self
            .last
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .ok_or_else(|| anyhow!("no date result to copy"))?;
        crate::clipboard::copy_text(&value)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for day in [0, 1, 20_000, -1, 19_000, 25_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
    }

    #[test]
    fn known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(weekday(0), "Thursday");
        assert_eq!(days_from_civil(2026, 10, 8) - days_from_civil(2026, 8, 28), 41);
    }

    #[test]
    fn parses_the_phrasings_people_type() {
        let base = days_from_civil(2026, 8, 28);
        for text in ["october 8", "oct 8", "8 october", "2026-10-08", "October 8, 2026"] {
            assert_eq!(
                parse_date(&text.to_lowercase(), base, Prefer::Future),
                Some(days_from_civil(2026, 10, 8)),
                "{text}"
            );
        }
    }

    #[test]
    fn bare_date_resolves_towards_the_question() {
        let base = days_from_civil(2026, 8, 28);
        // March has already passed in 2026, so "until" must mean 2027 and
        // "since" must mean the one earlier this year.
        assert_eq!(parse_date("march 1", base, Prefer::Future), Some(days_from_civil(2027, 3, 1)));
        assert_eq!(parse_date("march 1", base, Prefer::Past), Some(days_from_civil(2026, 3, 1)));
    }

    #[test]
    fn neither_direction_ever_answers_negatively() {
        for query in ["days until march 1", "days since march 1", "days until october 8"] {
            let text = eval_once(query).expect(query);
            assert!(!text.starts_with('-'), "{query} answered {text}");
        }
    }

    #[test]
    fn rejects_impossible_dates() {
        let base = days_from_civil(2026, 8, 28);
        assert_eq!(parse_date("february 30", base, Prefer::Future), None);
        assert_eq!(parse_date("2026-02-30", base, Prefer::Future), None);
        assert_eq!(parse_date("notamonth 8", base, Prefer::Future), None);
    }

    #[test]
    fn leap_day_is_accepted_only_in_leap_years() {
        let base = days_from_civil(2024, 1, 1);
        assert!(parse_date("2024-02-29", base, Prefer::Future).is_some());
        assert!(parse_date("2025-02-29", base, Prefer::Future).is_none());
    }
}

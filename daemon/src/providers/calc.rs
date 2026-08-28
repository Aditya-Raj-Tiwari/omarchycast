use crate::core::{Action, Item, Provider, Query};
use anyhow::{anyhow, Result};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROVIDER: &str = "calc";
/// Calculator results always sit above app matches — if the input parses as maths,
/// that is almost certainly what was meant.
const CALC_SCORE: i64 = 1_000_000;
const EVAL_BUDGET: Duration = Duration::from_millis(50);

thread_local! {
    static CTX: RefCell<fend_core::Context> = RefCell::new(fend_core::Context::new());
}

/// Seconds east of UTC for the current local time. fend needs this to answer
/// anything involving dates, and without it `today` and friends simply fail.
fn utc_offset_secs(now_secs: i64) -> i64 {
    // SAFETY: `localtime_r` writes into the `tm` we own and reads a `time_t` we own.
    unsafe {
        let time = now_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

/// fend is a full language and can be handed pathological input from a search box,
/// so every evaluation runs against a wall-clock budget.
struct Deadline(Instant);

impl fend_core::Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() > self.0
    }
}

pub struct CalcProvider {
    /// The result of the last successful evaluation, so activation doesn't re-run it.
    last: Mutex<Option<(String, String)>>,
}

impl CalcProvider {
    pub fn new() -> Arc<Self> {
        Arc::new(CalcProvider { last: Mutex::new(None) })
    }
}

/// Cheap pre-filter. Without it every app search would also be fed to fend, which
/// happily resolves bare words like `pi` or `c` into numbers and pollutes the list.
fn looks_like_maths(q: &str) -> bool {
    if q.len() < 3 || !q.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let has_operator = q.contains(['+', '*', '/', '^', '%', '(', ')', '=']);
    // A bare `-` is common in app names ("gnome-terminal"), so require spaces around it.
    let has_subtraction = q.contains(" - ");
    let lower = q.to_ascii_lowercase();
    let has_conversion = [" to ", " in ", " as "].iter().any(|k| lower.contains(k));
    has_operator || has_subtraction || has_conversion
}

fn evaluate(input: &str) -> Option<String> {
    let interrupt = Deadline(Instant::now() + EVAL_BUDGET);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;

    CTX.with(|ctx| {
        let mut ctx = ctx.borrow_mut();
        // Refreshed per evaluation: the daemon runs for days, and a stale "now"
        // would quietly give wrong answers to every date question.
        ctx.set_current_time_v1(now.as_millis() as u64, utc_offset_secs(now.as_secs() as i64));

        let result = fend_core::evaluate_with_interrupt(input, &mut ctx, &interrupt).ok()?;
        let main = result.get_main_result().trim().to_string();
        // fend echoes input it doesn't understand; a result identical to the query
        // carries no information.
        if main.is_empty() || main == input.trim() {
            return None;
        }
        Some(main)
    })
}

/// Exposed for the `omacast eval` debug subcommand.
pub fn eval_once(input: &str) -> Option<String> {
    evaluate(input)
}

impl Provider for CalcProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn query(&self, q: &Query) -> Vec<Item> {
        if !looks_like_maths(&q.trimmed) {
            return Vec::new();
        }
        let Some(result) = evaluate(&q.trimmed) else {
            return Vec::new();
        };

        if let Ok(mut last) = self.last.lock() {
            *last = Some((q.trimmed.clone(), result.clone()));
        }

        let mut item = Item::new(PROVIDER, "Calculator", "result".to_string(), result.clone());
        item.subtitle = Some(q.trimmed.clone());
        item.glyph = Some("=".to_string());
        item.accessory = Some("↵ copy".to_string());
        item.score = CALC_SCORE;
        vec![item]
    }

    fn activate(&self, _id: &str, action: Action) -> Result<()> {
        let (expression, result) = self
            .last
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .ok_or_else(|| anyhow!("no calculation to copy"))?;

        let payload = match action {
            Action::Primary => result,
            // Shift+Enter copies the whole working, not just the answer.
            Action::Secondary => format!("{expression} = {result}"),
        };
        crate::clipboard::copy_text(&payload)?;
        Ok(())
    }
}

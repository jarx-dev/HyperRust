//! Account balances of the paid providers and how long they last, plus the
//! per-dictation usage rows of `timings.jsonl`, for the overview page of the
//! settings window and the tray tooltip. Read-only requests; each key is sent
//! only to its own provider.

use std::time::Duration;

use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use serde_json::Value;

use crate::config::OPENROUTER_BASE_URL;
use crate::error::{Error, Result};

pub const ELEVENLABS_SUBSCRIPTION_URL: &str = "https://api.elevenlabs.io/v1/user/subscription";

/// ElevenLabs credits per minute of audio, measured on 15.10.2026 from the
/// account's usage analytics (credits / billed minutes of each Scribe model).
pub const SCRIBE_REALTIME_CREDITS_PER_MIN: f64 = 78.5;
pub const SCRIBE_BATCH_CREDITS_PER_MIN: f64 = 68.0;

const TIMEOUT: Duration = Duration::from_secs(10);
const DAY_S: f64 = 86_400.0;
/// Average month, to estimate when a monthly credit period began.
const MONTH_S: f64 = 30.44 * DAY_S;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ElevenLabsBalance {
    pub tier: String,
    pub used: u64,
    pub limit: u64,
    pub remaining: u64,
    /// Unix seconds of the next credit reset.
    pub reset_unix: Option<u64>,
    pub credits_per_min_realtime: f64,
    pub credits_per_min_batch: f64,
    /// Average credits per day since the current monthly period began.
    pub per_day: Option<f64>,
    /// When the credits run out at that pace; `None` when they last until the reset.
    pub runs_out_unix: Option<u64>,
}

impl ElevenLabsBalance {
    /// Parse `GET /v1/user/subscription`.
    pub fn from_subscription(v: &Value, now_unix: u64) -> Result<Self> {
        let num = |k: &str| v.get(k).and_then(Value::as_u64);
        let (Some(used), Some(limit)) = (num("character_count"), num("character_limit")) else {
            return Err(Error::InvalidResponse(crate::tr!("missing character_count or character_limit", "brak character_count lub character_limit")));
        };
        let remaining = limit.saturating_sub(used);
        let reset_unix = num("next_character_count_reset_unix");
        let monthly = v
            .get("character_refresh_period")
            .and_then(Value::as_str)
            .is_none_or(|p| p == "monthly_period");
        let per_day = reset_unix.filter(|_| monthly && used > 0).map(|reset| {
            let elapsed_days = (now_unix as f64 - (reset as f64 - MONTH_S)) / DAY_S;
            used as f64 / elapsed_days.max(1.0)
        });
        let runs_out_unix = per_day.and_then(|pace| {
            let at = now_unix + (remaining as f64 / pace * DAY_S) as u64;
            reset_unix.filter(|&reset| at < reset).map(|_| at)
        });
        Ok(Self {
            tier: v.get("tier").and_then(Value::as_str).unwrap_or_default().to_string(),
            used,
            limit,
            remaining,
            reset_unix,
            credits_per_min_realtime: SCRIBE_REALTIME_CREDITS_PER_MIN,
            credits_per_min_batch: SCRIBE_BATCH_CREDITS_PER_MIN,
            per_day,
            runs_out_unix,
        })
    }
}

/// Amounts in USD.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenRouterBalance {
    /// Account credits: purchased and what is left of them (`None` when the
    /// key may not read them).
    pub account_total: Option<f64>,
    pub account_remaining: Option<f64>,
    /// Spending limit of this key and what is left of it (`None` = no limit).
    pub key_limit: Option<f64>,
    pub key_remaining: Option<f64>,
    pub usage_daily: f64,
    pub usage_weekly: f64,
    pub usage_monthly: f64,
    /// What this key can still spend: the smaller of the account and key amounts.
    pub available: Option<f64>,
    /// Average spend per day over the last week.
    pub per_day: f64,
    /// Days `available` lasts at that pace.
    pub days_left: Option<f64>,
}

impl OpenRouterBalance {
    /// Combine `GET /key` with the optional `GET /credits`.
    pub fn from_responses(key: &Value, credits: Option<&Value>) -> Result<Self> {
        let data = key
            .get("data")
            .ok_or_else(|| Error::InvalidResponse(crate::tr!("missing data field in /key", "brak pola data w /key")))?;
        let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
        let credits = credits.and_then(|c| c.get("data"));
        let account_total = credits.and_then(|c| f(c, "total_credits"));
        let account_remaining = credits.and_then(|c| Some(f(c, "total_credits")? - f(c, "total_usage")?));
        let key_limit = f(data, "limit");
        let key_remaining = f(data, "limit_remaining");
        let available = match (account_remaining, key_remaining) {
            (Some(a), Some(k)) => Some(a.min(k)),
            (a, k) => a.or(k),
        }
        .map(|v| v.max(0.0));
        let usage_weekly = f(data, "usage_weekly").unwrap_or(0.0);
        let per_day = usage_weekly / 7.0;
        Ok(Self {
            account_total,
            account_remaining,
            key_limit,
            key_remaining,
            usage_daily: f(data, "usage_daily").unwrap_or(0.0),
            usage_weekly,
            usage_monthly: f(data, "usage_monthly").unwrap_or(0.0),
            available,
            per_day,
            days_left: available.filter(|_| per_day > 0.0).map(|a| a / per_day),
        })
    }
}

pub async fn elevenlabs(client: &Client, key: &str, now_unix: u64) -> Result<ElevenLabsBalance> {
    let v = get_json(client.get(ELEVENLABS_SUBSCRIPTION_URL).header("xi-api-key", key)).await?;
    ElevenLabsBalance::from_subscription(&v, now_unix)
}

pub async fn openrouter(client: &Client, key: &str) -> Result<OpenRouterBalance> {
    let (key_info, credits) = tokio::join!(
        get_json(client.get(format!("{OPENROUTER_BASE_URL}/key")).bearer_auth(key)),
        get_json(client.get(format!("{OPENROUTER_BASE_URL}/credits")).bearer_auth(key)),
    );
    if let Err(e) = &credits {
        tracing::debug!("OpenRouter /credits: {e}");
    }
    OpenRouterBalance::from_responses(&key_info?, credits.ok().as_ref())
}

async fn get_json(request: RequestBuilder) -> Result<Value> {
    let timed_out = |e: reqwest::Error| if e.is_timeout() { Error::Timeout(TIMEOUT) } else { e.into() };
    let resp = request.timeout(TIMEOUT).send().await.map_err(timed_out)?;
    let status = resp.status();
    let body = resp.text().await.map_err(timed_out)?;
    if !status.is_success() {
        return Err(Error::from_status(status.as_u16(), &body));
    }
    serde_json::from_str(&body).map_err(|e| Error::InvalidResponse(e.to_string()))
}

/// Windows keeps 127 UTF-16 units of a tray tooltip (plus the terminator).
pub const TOOLTIP_MAX: usize = 127;
const LOW: &str = "⚠ ";

/// Tray tooltip line: share of the credits left, about how much speech that
/// is at `credits_per_min`, and when they renew. Marked when under 10 % are
/// left or the current pace uses them up before the renewal.
pub fn elevenlabs_line(b: &ElevenLabsBalance, credits_per_min: f64, now_unix: u64) -> String {
    let percent = (b.remaining * 100).checked_div(b.limit).unwrap_or(0);
    let minutes = (b.remaining as f64 / credits_per_min) as u64;
    let mut line = format!("ElevenLabs: {percent}%, ≈{}", speech(minutes));
    if let Some(left) = b.reset_unix.and_then(|reset| reset.checked_sub(now_unix)).filter(|&left| left > 0) {
        let wait = wait(left);
        line.push_str(&crate::tr!(", renews in {wait}", ", odnowienie za {wait}"));
    }
    let low = b.remaining * 10 < b.limit || b.runs_out_unix.is_some();
    if low {
        line.insert_str(0, LOW);
    }
    line
}

/// Tray tooltip line for two ElevenLabs accounts (dictation moves to the
/// second one when the first fails): the share left on each and how much
/// speech both still cover. Marked when under 10 % of both together is left.
pub fn elevenlabs_pair_line(main: &ElevenLabsBalance, backup: &ElevenLabsBalance, credits_per_min: f64) -> String {
    let percent = |b: &ElevenLabsBalance| (b.remaining * 100).checked_div(b.limit).unwrap_or(0);
    let remaining = main.remaining + backup.remaining;
    let minutes = (remaining as f64 / credits_per_min) as u64;
    let mut line = format!("ElevenLabs: {}% + {}%, ≈{}", percent(main), percent(backup), speech(minutes));
    if remaining * 10 < main.limit + backup.limit {
        line.insert_str(0, LOW);
    }
    line
}

/// Tray tooltip line: what the key can still spend and for how many days at
/// last week's pace. Marked under 1 $ or a week.
pub fn openrouter_line(b: &OpenRouterBalance) -> String {
    let Some(available) = b.available else {
        let spent = dollars(b.usage_weekly);
        return crate::tr!("OpenRouter: {spent} $ spent in 7 days", "OpenRouter: wydano {spent} $ w 7 dni");
    };
    let mut line = format!("OpenRouter: {} $", dollars(available));
    if let Some(days) = b.days_left.filter(|&d| d < 1000.0) {
        line.push_str(&match days as u64 {
            0 => crate::tr!(" (<1 day)", " (<1 dzień)"),
            1 => crate::tr!(" (≈1 day)", " (≈1 dzień)"),
            n => crate::tr!(" (≈{n} days)", " (≈{n} dni)"),
        });
    }
    if available < 1.0 || b.days_left.is_some_and(|d| d < 7.0) {
        line.insert_str(0, LOW);
    }
    line
}

/// Join tooltip lines within `max` UTF-16 units: `optional` lines follow only
/// while they fit, `required` lines that do not fit are cut with an ellipsis.
pub fn fit_tooltip(required: &[String], optional: &[String], max: usize) -> String {
    let len = |s: &str| s.encode_utf16().count();
    let mut text = required.join("\n");
    if len(&text) > max {
        while len(&text) + 1 > max {
            text.pop();
        }
        text.push('…');
        return text;
    }
    for line in optional {
        if len(&text) + 1 + len(line) <= max {
            text.push('\n');
            text.push_str(line);
        }
    }
    text
}

fn speech(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) if h < 10 => format!("{h} h {m} min"),
        (h, _) => format!("{h} h"),
    }
}

/// Polish "dni" for 2+ days is the same word for every count.
fn wait(seconds: u64) -> String {
    match seconds / 86_400 {
        0 => format!("{} h", seconds.div_ceil(3_600)),
        1 => crate::tr!("1 day", "1 dzień"),
        days => crate::tr!("{days} days", "{days} dni"),
    }
}

fn dollars(amount: f64) -> String {
    let text = format!("{amount:.2}");
    if crate::i18n::is_polish() {
        text.replace('.', ",")
    } else {
        text
    }
}

/// One dictation of the latency log, as much as the usage overview needs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UsageRow {
    /// Unix seconds.
    pub ts: u64,
    pub audio_ms: u64,
    pub stt: String,
    pub streamed: bool,
    pub llm: String,
    pub outcome: String,
    pub release_to_paste_ms: Option<u64>,
}

/// Rows of `timings.jsonl` logged at or after `since_unix`, oldest first.
/// Lines that do not parse (e.g. one cut short by a crash) are skipped.
pub fn usage_rows(log: &str, since_unix: u64) -> Vec<UsageRow> {
    let mut rows: Vec<UsageRow> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| {
            let ts = v.get("ts")?.as_u64()?;
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            (ts >= since_unix).then(|| UsageRow {
                ts,
                audio_ms: v.get("audio_ms").and_then(Value::as_u64).unwrap_or(0),
                stt: text("stt"),
                streamed: v.get("streamed").and_then(Value::as_bool).unwrap_or(false),
                llm: text("llm"),
                outcome: text("outcome"),
                release_to_paste_ms: v.get("release_to_paste_ms").and_then(Value::as_u64),
            })
        })
        .collect();
    rows.sort_by_key(|r| r.ts);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_792_000_000;

    #[test]
    fn elevenlabs_estimates_pace_within_the_period() {
        // 10 days into the period, 1000 of 10 000 credits used: 100 per day lasts past the reset.
        let reset = NOW + (MONTH_S - 10.0 * DAY_S) as u64;
        let v = json!({"tier": "free", "character_count": 1000, "character_limit": 10000,
                       "next_character_count_reset_unix": reset, "character_refresh_period": "monthly_period"});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        assert_eq!((b.tier.as_str(), b.remaining), ("free", 9000));
        assert!((b.per_day.unwrap() - 100.0).abs() < 0.01);
        assert_eq!(b.runs_out_unix, None);

        // 9000 used in 10 days: 900 per day runs out in about a day, before the reset.
        let v = json!({"character_count": 9000, "character_limit": 10000, "next_character_count_reset_unix": reset});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        let out = b.runs_out_unix.unwrap();
        assert!(out > NOW && out < NOW + 2 * 86_400);
    }

    #[test]
    fn elevenlabs_without_usage_or_counts() {
        let v = json!({"character_count": 0, "character_limit": 10000, "next_character_count_reset_unix": NOW + 1000});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        assert_eq!((b.per_day, b.runs_out_unix, b.remaining), (None, None, 10000));
        assert!(ElevenLabsBalance::from_subscription(&json!({"detail": "x"}), NOW).is_err());
    }

    #[test]
    fn openrouter_takes_the_smaller_of_account_and_key() {
        let key = json!({"data": {"limit": 5.0, "limit_remaining": 4.28, "usage": 0.72,
                                   "usage_daily": 0.04, "usage_weekly": 0.07, "usage_monthly": 0.07}});
        let credits = json!({"data": {"total_credits": 50.0, "total_usage": 48.77}});
        let b = OpenRouterBalance::from_responses(&key, Some(&credits)).unwrap();
        let available = b.available.unwrap();
        assert!((available - 1.23).abs() < 1e-9);
        assert!((b.days_left.unwrap() - available / 0.01).abs() < 1e-6);

        // No /credits access and no key limit: nothing to spend from is known.
        let key = json!({"data": {"limit": null, "limit_remaining": null, "usage_weekly": 0.0}});
        let b = OpenRouterBalance::from_responses(&key, None).unwrap();
        assert_eq!((b.available, b.days_left, b.per_day), (None, None, 0.0));
        assert!(OpenRouterBalance::from_responses(&json!({"error": {}}), None).is_err());
    }

    #[test]
    fn tooltip_lines_show_what_is_left_and_warn_when_low() {
        crate::i18n::set(crate::i18n::UiLanguage::Pl);
        let reset = NOW + 18 * 86_400 + 600;
        let v = json!({"character_count": 3600, "character_limit": 10000, "next_character_count_reset_unix": reset,
                       "character_refresh_period": "monthly_period"});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        // 6400 credits at 80 per minute: 80 minutes of speech.
        assert_eq!(elevenlabs_line(&b, 80.0, NOW), "ElevenLabs: 64%, ≈1 h 20 min, odnowienie za 18 dni");

        let v = json!({"character_count": 9500, "character_limit": 10000, "next_character_count_reset_unix": NOW + 7200});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        assert_eq!(elevenlabs_line(&b, 80.0, NOW), "⚠ ElevenLabs: 5%, ≈6 min, odnowienie za 2 h");

        // The first account used up, the second one full: 10 000 credits left.
        let used_up = json!({"character_count": 10000, "character_limit": 10000});
        let main = ElevenLabsBalance::from_subscription(&used_up, NOW).unwrap();
        let full = json!({"character_count": 0, "character_limit": 10000});
        let backup = ElevenLabsBalance::from_subscription(&full, NOW).unwrap();
        assert_eq!(elevenlabs_pair_line(&main, &backup, 80.0), "ElevenLabs: 0% + 100%, ≈2 h 5 min");
        assert_eq!(elevenlabs_pair_line(&main, &b, 80.0), "⚠ ElevenLabs: 0% + 5%, ≈6 min");

        let key = json!({"data": {"limit": null, "limit_remaining": null, "usage_weekly": 0.7}});
        let credits = json!({"data": {"total_credits": 10.0, "total_usage": 6.79}});
        let b = OpenRouterBalance::from_responses(&key, Some(&credits)).unwrap();
        assert_eq!(openrouter_line(&b), "OpenRouter: 3,21 $ (≈32 dni)");
        let credits = json!({"data": {"total_credits": 10.0, "total_usage": 9.5}});
        let b = OpenRouterBalance::from_responses(&key, Some(&credits)).unwrap();
        assert_eq!(openrouter_line(&b), "⚠ OpenRouter: 0,50 $ (≈5 dni)");
        let b = OpenRouterBalance::from_responses(&key, None).unwrap();
        assert_eq!(openrouter_line(&b), "OpenRouter: wydano 0,70 $ w 7 dni");

        crate::i18n::set(crate::i18n::UiLanguage::En);
        assert_eq!(openrouter_line(&b), "OpenRouter: 0.70 $ spent in 7 days");
        let v = json!({"character_count": 3600, "character_limit": 10000, "next_character_count_reset_unix": reset});
        let b = ElevenLabsBalance::from_subscription(&v, NOW).unwrap();
        assert_eq!(elevenlabs_line(&b, 80.0, NOW), "ElevenLabs: 64%, ≈1 h 20 min, renews in 18 days");
    }

    #[test]
    fn tooltip_fits_the_windows_limit() {
        let req = ["HyperRust — Tryb: Prompt".to_string(), "ElevenLabs: 64%".to_string()];
        let opt = ["Skrót: Ctrl+Shift+Space".to_string(), "x".repeat(200), "Wersja: abc".to_string()];
        let text = fit_tooltip(&req, &opt, TOOLTIP_MAX);
        assert_eq!(text, "HyperRust — Tryb: Prompt\nElevenLabs: 64%\nSkrót: Ctrl+Shift+Space\nWersja: abc");

        let long = ["ą".repeat(200)];
        let text = fit_tooltip(&long, &opt, TOOLTIP_MAX);
        assert_eq!(text.encode_utf16().count(), TOOLTIP_MAX);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn usage_rows_skip_old_and_broken_lines() {
        let log = concat!(
            r#"{"ts":100,"audio_ms":5000,"stt":"ElevenLabs","streamed":true,"llm":"endpoint:m","outcome":"ok","release_to_paste_ms":1400}"#,
            "\n",
            r#"{"ts":300,"audio_ms":2000,"stt":"OpenRouter","streamed":false,"llm":"","outcome":"stt_error","release_to_paste_ms":null}"#,
            "\n{\"ts\":200,\"audio_ms\":10\n",
            r#"{"ts":200,"audio_ms":1000,"stt":"ElevenLabs","streamed":false,"llm":"","outcome":"ok"}"#,
            "\n",
        );
        let rows = usage_rows(log, 150);
        assert_eq!(rows.iter().map(|r| r.ts).collect::<Vec<_>>(), [200, 300]);
        assert_eq!((rows[0].audio_ms, rows[0].release_to_paste_ms), (1000, None));
        assert_eq!((rows[1].stt.as_str(), rows[1].outcome.as_str()), ("OpenRouter", "stt_error"));
    }
}

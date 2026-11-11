//! Ranking of post-processing models. A daily job on a private server
//! (`examples/ranking.rs`, run by `scripts/hyperrust-ranking.timer`) tests new
//! OpenRouter chat models on a fixed set of dictations with the app's request,
//! a blind judge scores the results, every test is appended to a log and the
//! ranking derived from that log is published as `ranking.json` next to the
//! update manifest, and without the judge's remarks on GitHub ([`GITHUB_URL`]).
//! The app only downloads and shows it.
//!
//! What gets tested is decided here ([`plan`]): a new model gets one full test
//! (outputs scored by the judge), a second one when the first came in its busy
//! days after release ([`SETTLE_DAYS`]), the best few are re-timed weekly because
//! speed changes with providers and load, and a new agent prompt sends the best
//! few through a full test again. "Best" is the ranking order ([`order`]):
//! speed and cost among the models good enough.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::catalog::{CatalogModel, Price, NEW_DAYS};
use crate::error::{Error, Result};

/// Published next to `latest.json` on the update server.
pub const FILE_NAME: &str = "ranking.json";
/// OpenRouter routing suffix of every tested request: providers sorted by
/// throughput (`:nitro`), the fastest route for short answers.
pub const ROUTING: &str = ":nitro";
/// New models are tested up to this output price (USD per 1M tokens).
pub const MAX_OUTPUT_USD: f64 = 5.0;
/// Models scored at least this are ranked by speed and cost, above the rest.
pub const GOOD_SCORE: f64 = 8.0;
/// The best this many are re-timed every [`RETIME_DAYS`].
pub const RETIME_TOP: usize = 5;
pub const RETIME_DAYS: u64 = 7;
/// After an agent prompt change, the best this many are tested again.
pub const RETEST_TOP: usize = 6;
/// A model tested in its first this many days after release, while providers
/// are often overloaded, gets one more full test once it is this old.
pub const SETTLE_DAYS: u64 = 2;
/// The app warns when the ranking is older than this.
pub const STALE_DAYS: u64 = 2;
/// Earlier timings kept per model in the published ranking.
const EARLIER: usize = 6;
const DAY: u64 = 86_400;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Outputs scored by the judge.
    Full,
    /// Timing only.
    Speed,
}

/// One test of one model: a line of `measurements.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Unix seconds.
    pub at: u64,
    /// Id as the catalog lists it (`vendor/model`).
    pub model: String,
    pub name: String,
    /// What was sent: the id with [`ROUTING`], and the reasoning effort.
    pub request_model: String,
    pub effort: String,
    pub kind: Kind,
    /// Fingerprints of the agent prompt and of the dictation set.
    pub prompt_sha: String,
    pub set_sha: String,
    pub cases: usize,
    pub errors: usize,
    /// Full response time of the successful requests.
    pub median_ms: Option<u64>,
    pub p90_ms: Option<u64>,
    /// Median USD per dictation, as OpenRouter billed it.
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub judge: String,
    /// Mean judge score 0–10; failed requests count as 0. None for timing
    /// runs and for tests stopped early.
    #[serde(default)]
    pub score: Option<f64>,
    /// Dictations scored 9 or more.
    #[serde(default)]
    pub good: usize,
    /// Judge remarks on the dictations scored below 8.
    #[serde(default)]
    pub issues: Vec<String>,
    /// Why a test stopped early.
    #[serde(default)]
    pub note: String,
}

/// `ranking.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    /// Unix seconds of the run that wrote it.
    pub generated_at: u64,
    /// Agent prompt the scores belong to (see [`crate::history::prompt_sha`]).
    pub prompt_sha: String,
    pub judge: String,
    /// Dictations per test.
    pub cases: usize,
    /// Best first.
    pub models: Vec<RankedModel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedModel {
    pub model: String,
    pub name: String,
    /// Settings that reproduce the test: OpenRouter model id and effort.
    pub use_model: String,
    pub effort: String,
    pub score: Option<f64>,
    pub good: usize,
    pub cases: usize,
    pub errors: usize,
    /// Unix seconds of the full test.
    pub tested_at: u64,
    /// The latest timing (full test or re-timing).
    pub median_ms: Option<u64>,
    pub p90_ms: Option<u64>,
    pub timed_at: u64,
    pub cost_usd: Option<f64>,
    /// Earlier timings, newest first.
    pub earlier: Vec<Timing>,
    /// Scored with another agent prompt than [`Ranking::prompt_sha`].
    pub other_prompt: bool,
    /// Still in OpenRouter's catalog.
    pub available: bool,
    pub issues: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub at: u64,
    pub median_ms: u64,
}

/// The OpenRouter model id a test sends for a catalog id.
pub fn request_model(model: &str) -> String {
    format!("{model}{ROUTING}")
}

/// Median and 90th percentile.
pub fn spread(values: &[u64]) -> (Option<u64>, Option<u64>) {
    let mut v = values.to_vec();
    v.sort_unstable();
    if v.is_empty() {
        return (None, None);
    }
    let p90 = v[(v.len() * 9).div_ceil(10) - 1];
    (Some(v[v.len() / 2]), Some(p90))
}

pub fn median_f64(values: &[f64]) -> Option<f64> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// The ranking the log implies: per model its latest full test (with the
/// current prompt when there is one) and its latest timing.
pub fn build(
    log: &[Measurement],
    prompt_sha: &str,
    judge: &str,
    cases: usize,
    available: &dyn Fn(&str) -> bool,
    now: u64,
) -> Ranking {
    let mut by_model: BTreeMap<&str, Vec<&Measurement>> = BTreeMap::new();
    for m in log {
        by_model.entry(&m.model).or_default().push(m);
    }
    let mut models: Vec<RankedModel> = by_model
        .into_iter()
        .filter_map(|(model, tests)| {
            let full = || tests.iter().copied().filter(|m| m.kind == Kind::Full);
            let quality = full().rfind(|m| m.prompt_sha == prompt_sha).or_else(|| full().next_back())?;
            let timed: Vec<&Measurement> = tests.iter().copied().filter(|m| m.median_ms.is_some()).collect();
            let speed = timed.last().copied().unwrap_or(quality);
            Some(RankedModel {
                model: model.to_string(),
                name: quality.name.clone(),
                use_model: quality.request_model.clone(),
                effort: quality.effort.clone(),
                score: quality.score,
                good: quality.good,
                cases: quality.cases,
                errors: quality.errors,
                tested_at: quality.at,
                median_ms: speed.median_ms,
                p90_ms: speed.p90_ms,
                timed_at: speed.at,
                cost_usd: speed.cost_usd.or(quality.cost_usd),
                earlier: timed
                    .iter()
                    .rev()
                    .skip(1)
                    .take(EARLIER)
                    .filter_map(|m| Some(Timing { at: m.at, median_ms: m.median_ms? }))
                    .collect(),
                other_prompt: quality.prompt_sha != prompt_sha,
                available: available(model),
                issues: quality.issues.iter().take(3).cloned().collect(),
                note: quality.note.clone(),
            })
        })
        .collect();
    models.sort_by(|a, b| a.other_prompt.cmp(&b.other_prompt).then_with(|| order(a, b)));
    Ranking { generated_at: now, prompt_sha: prompt_sha.into(), judge: judge.into(), cases, models }
}

/// The ranking order, whatever the prompt: models scored [`GOOD_SCORE`] or
/// more by median time times cost (twice as fast counts as much as half the
/// price), then the rest by score, the unscored last.
pub fn order(a: &RankedModel, b: &RankedModel) -> Ordering {
    let good = |m: &RankedModel| m.score.is_some_and(|s| s >= GOOD_SCORE);
    let time_cost = |m: &RankedModel| match (m.median_ms, m.cost_usd) {
        (Some(ms), Some(usd)) => ms as f64 * usd,
        _ => f64::INFINITY,
    };
    good(b)
        .cmp(&good(a))
        .then_with(|| if good(a) { time_cost(a).total_cmp(&time_cost(b)) } else { Ordering::Equal })
        .then_with(|| b.score.unwrap_or(-1.0).total_cmp(&a.score.unwrap_or(-1.0)))
        .then_with(|| a.median_ms.unwrap_or(u64::MAX).cmp(&b.median_ms.unwrap_or(u64::MAX)))
}

/// What a run tests.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Catalog ids for a full test: `forced`, the best of an earlier prompt,
    /// models never tested, then models tested before they settled.
    pub full: Vec<String>,
    /// Catalog ids to re-time.
    pub retime: Vec<String>,
}

/// A model the job tests on its own: a chat model added in the last
/// [`NEW_DAYS`] with an output price up to [`MAX_OUTPUT_USD`].
pub fn eligible(m: &CatalogModel, now: u64) -> bool {
    let cheap = matches!(m.price, Some(Price::Tokens { output, .. }) if output <= MAX_OUTPUT_USD);
    m.chat && cheap && m.created.is_some_and(|t| t >= now.saturating_sub(NEW_DAYS * DAY))
}

pub fn plan(catalog: &[CatalogModel], log: &[Measurement], prompt_sha: &str, forced: &[String], now: u64) -> Plan {
    let listed: HashSet<&str> = catalog.iter().filter(|m| m.chat).map(|m| m.id.as_str()).collect();
    let tested: HashSet<&str> = log.iter().map(|m| m.model.as_str()).collect();
    let mut full: Vec<String> = Vec::new();
    let add = |id: &str, full: &mut Vec<String>| {
        if !full.iter().any(|m| m == id) {
            full.push(id.to_string());
        }
    };
    for id in forced {
        add(id, &mut full);
    }

    // The best in the ranking order by their latest test, whatever the prompt.
    let ranking = build(log, prompt_sha, "", 0, &|id| listed.contains(id), now);
    let mut best: Vec<&RankedModel> = ranking.models.iter().filter(|m| m.score.is_some()).collect();
    best.sort_by(|a, b| order(a, b));
    for m in best.iter().take(RETEST_TOP) {
        if m.other_prompt && m.available {
            add(&m.model, &mut full);
        }
    }
    for m in catalog.iter().filter(|m| eligible(m, now) && !tested.contains(m.id.as_str())) {
        add(&m.id, &mut full);
    }
    // Speed and errors right after release say more about the rush than the
    // model, whatever the first test found (stopped early or unavailable too).
    for m in catalog.iter().filter(|m| eligible(m, now)) {
        let Some(settled) = m.created.map(|t| t + SETTLE_DAYS * DAY) else { continue };
        let last_full = log.iter().filter(|t| t.model == m.id && t.kind == Kind::Full).map(|t| t.at).max();
        if now >= settled && last_full.is_some_and(|at| at < settled) {
            add(&m.id, &mut full);
        }
    }

    let retime = ranking
        .models
        .iter()
        .filter(|m| !m.other_prompt && m.available && m.score.is_some())
        .take(RETIME_TOP)
        .filter(|m| now.saturating_sub(m.timed_at) >= RETIME_DAYS * DAY && !full.contains(&m.model))
        .map(|m| m.model.clone())
        .collect();
    Plan { full, retime }
}

/// Public copy of the ranking (judge remarks removed: they quote private
/// dictations), uploaded by `scripts/ranking-nuc.sh` after each run.
pub const GITHUB_URL: &str = "https://github.com/jarx-dev/HyperRust/releases/download/ranking/ranking.json";

/// `ranking.json` next to the update manifest at `update_url`; for GitHub
/// Releases the public copy ([`GITHUB_URL`]).
pub fn url(update_url: &str) -> Result<Url> {
    let manifest = crate::update::validate_private_url(update_url, &crate::tr!("update", "aktualizacji"))?;
    let joined = if manifest.host_str() == Some("github.com") {
        GITHUB_URL.to_string()
    } else {
        manifest
            .join(FILE_NAME)
            .map_err(|e| Error::Other(crate::tr!("ranking address: {e}", "adres rankingu: {e}")))?
            .to_string()
    };
    crate::update::validate_private_url(&joined, &crate::tr!("ranking", "rankingu"))
}

pub fn parse(body: &[u8]) -> Result<Ranking> {
    serde_json::from_slice(body).map_err(|e| Error::InvalidResponse(format!("ranking: {e}")))
}

pub async fn fetch(client: &Client, url: &Url) -> Result<Ranking> {
    let resp = client
        .get(url.clone())
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .timeout(FETCH_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        return Err(Error::from_status(status.as_u16(), &String::from_utf8_lossy(&body)));
    }
    parse(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_000_000;

    fn measured(model: &str, kind: Kind, at: u64, prompt: &str, score: Option<f64>, median: u64) -> Measurement {
        Measurement {
            at,
            model: model.into(),
            name: model.into(),
            request_model: request_model(model),
            effort: "none".into(),
            kind,
            prompt_sha: prompt.into(),
            set_sha: "set".into(),
            cases: 24,
            errors: 0,
            median_ms: Some(median),
            p90_ms: Some(median * 2),
            cost_usd: Some(0.0001),
            judge: "opus".into(),
            score,
            good: 0,
            issues: Vec::new(),
            note: String::new(),
        }
    }

    fn listed(id: &str, days_ago: u64, output_usd: f64) -> CatalogModel {
        CatalogModel {
            id: id.into(),
            name: id.into(),
            created: Some(NOW - days_ago * DAY),
            owner: id.split('/').next().unwrap().into(),
            price: Some(Price::Tokens { input: 0.1, output: output_usd }),
            chat: true,
        }
    }

    #[test]
    fn spread_takes_median_and_p90() {
        assert_eq!(spread(&[]), (None, None));
        assert_eq!(spread(&[5]), (Some(5), Some(5)));
        let ms: Vec<u64> = (1..=10).rev().collect();
        assert_eq!(spread(&ms), (Some(6), Some(9)));
        assert_eq!(median_f64(&[0.3, 0.1, 0.2]), Some(0.2));
    }

    #[test]
    fn ranking_prefers_current_prompt_and_keeps_timing_history() {
        let log = [
            measured("a/fast", Kind::Full, NOW - 9 * DAY, "p1", Some(8.4), 400),
            measured("a/fast", Kind::Speed, NOW - 2 * DAY, "p1", None, 450),
            measured("b/good", Kind::Full, NOW - 9 * DAY, "p1", Some(9.0), 2600),
            measured("b/good", Kind::Full, NOW - DAY, "p2", Some(8.8), 2500),
            measured("c/old", Kind::Full, NOW - 9 * DAY, "p1", Some(9.5), 900),
        ];
        let r = build(&log, "p2", "opus", 24, &|id| id != "c/old", NOW);
        let ids: Vec<&str> = r.models.iter().map(|m| m.model.as_str()).collect();
        // Scores with the current prompt come first, whatever the old ones were.
        assert_eq!(ids, ["b/good", "a/fast", "c/old"]);
        let good = &r.models[0];
        assert_eq!((good.score, good.tested_at, good.median_ms, good.other_prompt), (Some(8.8), NOW - DAY, Some(2500), false));
        assert_eq!(good.earlier, [Timing { at: NOW - 9 * DAY, median_ms: 2600 }]);
        assert!(!r.models[2].available);
        let fast = &r.models[1];
        assert_eq!((fast.score, fast.median_ms, fast.timed_at, fast.other_prompt), (Some(8.4), Some(450), NOW - 2 * DAY, true));
        assert_eq!(fast.use_model, "a/fast:nitro");
    }

    #[test]
    fn ranking_puts_fast_cheap_good_models_first() {
        let priced = |model: &str, score: Option<f64>, median: u64, usd: f64| Measurement {
            cost_usd: Some(usd),
            ..measured(model, Kind::Full, NOW, "p", score, median)
        };
        let log = [
            priced("a/best-slow", Some(9.5), 3000, 0.001),
            priced("b/fast-cheap", Some(8.2), 500, 0.0001),
            priced("c/fast-pricey", Some(8.0), 400, 0.002),
            priced("d/weak-fastest", Some(7.9), 300, 0.00005),
            priced("e/weak", Some(6.0), 1000, 0.0001),
            priced("f/stopped", None, 9000, 0.0001),
        ];
        let r = build(&log, "p", "opus", 24, &|_| true, NOW);
        let ids: Vec<&str> = r.models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(ids, ["b/fast-cheap", "c/fast-pricey", "a/best-slow", "d/weak-fastest", "e/weak", "f/stopped"]);
    }

    #[test]
    fn plan_tests_new_models_once_and_retimes_the_best_weekly() {
        let catalog = [
            listed("a/tested", 10, 0.5),
            listed("b/new", 3, 0.7),
            listed("c/pricey", 3, 20.0),
            listed("d/ancient", 90, 0.2),
            CatalogModel { chat: false, ..listed("e/image", 1, 0.1) },
        ];
        let log = [measured("a/tested", Kind::Full, NOW - 8 * DAY, "p", Some(8.0), 500)];
        let p = plan(&catalog, &log, "p", &[], NOW);
        assert_eq!(p.full, ["b/new"]);
        assert_eq!(p.retime, ["a/tested"]);

        // Timed within the week: nothing to re-time; a model tested before is
        // not tested again, even when its test failed.
        let mut log = log.to_vec();
        log.push(measured("a/tested", Kind::Speed, NOW - DAY, "p", None, 450));
        log.push(Measurement { score: None, note: "przerwany".into(), ..measured("b/new", Kind::Full, NOW, "p", None, 9000) });
        assert_eq!(plan(&catalog, &log, "p", &[], NOW), Plan::default());
    }

    #[test]
    fn plan_tests_a_model_again_once_it_has_settled_after_release() {
        let catalog = [listed("a/rushed", 3, 0.5), listed("b/settled", 3, 0.5), listed("c/young", 1, 0.5)];
        let mut log = vec![
            // Too slow on its first day: stopped early, no score.
            Measurement { score: None, note: "przerwany".into(), ..measured("a/rushed", Kind::Full, NOW - 3 * DAY + 20 * 3600, "p", None, 9000) },
            measured("b/settled", Kind::Full, NOW - DAY, "p", Some(8.5), 500),
            measured("c/young", Kind::Full, NOW - DAY + 5 * 3600, "p", Some(8.1), 900),
        ];
        let p = plan(&catalog, &log, "p", &[], NOW);
        // c/young is tested again tomorrow.
        assert_eq!(p, Plan { full: vec!["a/rushed".into()], retime: Vec::new() });

        log.push(measured("a/rushed", Kind::Full, NOW, "p", Some(8.6), 600));
        assert_eq!(plan(&catalog, &log, "p", &[], NOW), Plan::default());
        assert_eq!(plan(&catalog, &log, "p", &[], NOW + DAY).full, ["c/young"]);
    }

    #[test]
    fn plan_retests_the_best_after_a_prompt_change() {
        let catalog: Vec<CatalogModel> = (0..8).map(|i| listed(&format!("v/m{i}"), 100, 0.5)).collect();
        let log: Vec<Measurement> = (0..8)
            .map(|i| measured(&format!("v/m{i}"), Kind::Full, NOW - 30 * DAY, "old", Some(9.0 - i as f64 * 0.5), 500))
            .collect();
        let forced = ["x/forced".to_string(), "v/m1".to_string()];
        let p = plan(&catalog, &log, "new", &forced, NOW);
        assert_eq!(p.full, ["x/forced", "v/m1", "v/m0", "v/m2", "v/m3", "v/m4", "v/m5"]);
        // Old-prompt scores are not re-timed; they are tested in full instead.
        assert!(p.retime.is_empty());
    }

    #[test]
    fn url_sits_next_to_the_manifest() {
        let u = url("http://100.101.102.103:8790/latest.json").unwrap();
        assert_eq!(u.as_str(), "http://100.101.102.103:8790/ranking.json");
        assert!(url("http://example.com/latest.json").is_err());
        assert_eq!(url(crate::update::GITHUB_UPDATE_URL).unwrap().as_str(), GITHUB_URL);
    }

    #[test]
    fn ranking_round_trips_through_json() {
        let log = [measured("a/fast", Kind::Full, NOW, "p", Some(8.4), 400)];
        let r = build(&log, "p", "opus", 24, &|_| true, NOW);
        let body = serde_json::to_vec(&r).unwrap();
        assert_eq!(parse(&body).unwrap(), r);
        assert!(parse(b"<html>").is_err());
    }
}

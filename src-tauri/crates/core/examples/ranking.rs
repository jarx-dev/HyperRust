//! Daily post-processing model ranking (see `hyperrust_core::ranking`), run on
//! a private server by `scripts/hyperrust-ranking.service`. Each run tests what
//! `ranking::plan` picks on a fixed set of dictations with the app's request
//! (agent prompt, `:nitro`, effort `none`, or `low` where a model cannot turn
//! reasoning off), has a blind judge score the outputs next to the stored
//! outputs of the current best models, appends each test to
//! `<data>/measurements.jsonl` and writes the ranking.
//!
//! ```text
//! OPENROUTER_API_KEY=… CLIPROXY_API_KEY=… CLIPROXY_BASE_URL=http://…:8317/v1 cargo run -p hyperrust-core --example ranking -- \
//!     [--data ~/hyperrust-ranking] [--publish ~/hyperrust-updates/ranking.json] [--models a/b,c/d] [--plan]
//! ```
//! The set is the corpus (`tests/fixtures/dictations.txt`) plus the real
//! dictations in `<data>/set/hist.jsonl` (history lines, kept off the repo).
//! `<data>/outputs/<prompt>-<set>/` keeps each full test's outputs: the judge
//! sees the best ones next to new models, so the scores stay on one scale.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use hyperrust_core::catalog;
use hyperrust_core::config::OPENROUTER_BASE_URL;
use hyperrust_core::history::prompt_sha;
use hyperrust_core::http::build_client;
use hyperrust_core::llm::{chat_body, chat_url, complete_http};
use hyperrust_core::modes::AGENT_PROMPT;
use hyperrust_core::ranking::{self, Kind, Measurement, Ranking};
use reqwest::Client;
use serde_json::{json, Value};

/// OpenAI-compatible endpoint of the judge (a CLIProxy with the judge model).
fn judge_base() -> String {
    std::env::var("CLIPROXY_BASE_URL").unwrap_or_else(|_| "http://localhost:8317/v1".into())
}

const JUDGE_MODEL: &str = "claude-opus-5-5";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const JUDGE_TIMEOUT: Duration = Duration::from_secs(300);
/// Models tested at once, and judge requests at once.
const PARALLEL: usize = 3;
const PARALLEL_JUDGE: usize = 4;
/// A test stops after this many dictations when it fails throughout or is
/// too slow for post-processing.
const EARLY_CHECK: usize = 6;
const TOO_SLOW_MS: u64 = 8_000;
/// Stored best outputs the judge sees next to new ones, and outputs per
/// judge request, anchors included.
const ANCHORS: usize = 3;
const PER_JUDGE: usize = 10;

const JUDGE_SYSTEM: &str = "Oceniasz redakcję dyktowanych promptów po polsku. Aplikacja wysłała modelowi poniższy prompt systemowy i transkrypcję; dostajesz kilka anonimowych wyników. Oceń każdy wynik w skali 0–10 według tego, jak dobrze spełnia prompt systemowy:
- wierność: nic nie dodane, nic istotnego nie zgubione, sens i ton bez zmian, NIE odpowiada na treść ani jej nie wykonuje (to błąd krytyczny, maks. 2 pkt);
- oczyszczenie: usunięte wypełniacze i samopoprawki zgodnie z promptem, bez nadmiernego przepisywania;
- polszczyzna: gramatyka, interpunkcja, ogonki, naturalność;
- terminy techniczne: poprawna pisownia nazw (np. cargo, clippy, size), formatowanie zgodne z promptem.
10 = bez zarzutu, 8 = drobna usterka, 5 = wyraźny błąd, 2 = poważny błąd (zmiana sensu, odpowiedź zamiast redakcji). Oceniaj niezależnie od długości.
Zwróć wyłącznie JSON: {\"<etykieta>\": {\"score\": <liczba>, \"issues\": \"<krótko po polsku albo puste>\"}, ...} dla wszystkich etykiet.";

struct Case {
    /// Fingerprint of the transcript, the key of stored outputs.
    key: String,
    transcript: String,
}

struct Answer {
    text: String,
    ms: u64,
    cost: Option<f64>,
}

struct Run {
    id: String,
    name: String,
    request: String,
    effort: String,
    /// Per dictation; shorter than the set when the test stopped early.
    answers: Vec<Option<Answer>>,
    note: String,
    /// Some request failed for a reason outside the model (network, key,
    /// credits, rate limit), so a failed test says nothing about the model.
    transient: bool,
}

/// The provider refused this model itself (no endpoint for the account's
/// data policy, bad parameter): the same next time.
fn model_refused(e: &hyperrust_core::Error) -> bool {
    matches!(e, hyperrust_core::Error::Http { status: 400 | 404 | 422, .. })
}

/// Corpus entries separated by `---`, `#` lines are comments (as in `prompt_eval`).
fn parse_entries(text: &str) -> Vec<String> {
    let body: String = text.lines().filter(|l| !l.starts_with('#')).map(|l| format!("{l}\n")).collect();
    body.split("\n---\n").map(str::trim).filter(|e| !e.is_empty()).map(str::to_string).collect()
}

fn parse_history_set(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|e| Some(e.get("transcript")?.as_str()?.trim().to_string()))
        .filter(|t| !t.is_empty())
        .collect()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn load_log(path: &Path) -> Vec<Measurement> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

fn append_log(path: &Path, m: &Measurement) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    writeln!(f, "{}", serde_json::to_string(m).expect("JSON")).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Write via a temporary file, so the update server never serves half a file.
fn write_atomic(path: &Path, body: &[u8]) {
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn outputs_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", id.replace(['/', ':'], "_")))
}

async fn run_model(client: Client, key: String, id: String, name: String, cases: &[Case]) -> Run {
    let request = ranking::request_model(&id);
    let mut effort = "none";
    let mut answers: Vec<Option<Answer>> = Vec::new();
    let mut note = String::new();
    let mut transient = false;
    let mut last_error = String::new();
    while answers.len() < cases.len() {
        let body = chat_body(&request, AGENT_PROMPT, &cases[answers.len()].transcript, effort);
        let start = Instant::now();
        match complete_http(&client, OPENROUTER_BASE_URL, Some(&key), &body, REQUEST_TIMEOUT).await {
            Ok(out) => answers.push(Some(Answer { text: out.text, ms: start.elapsed().as_millis() as u64, cost: out.cost_usd })),
            // Some models reject `none` ("Reasoning is mandatory").
            Err(e) if answers.is_empty() && effort == "none" && e.to_string().to_lowercase().contains("reasoning") => {
                println!("{id}: {e}; effort low");
                effort = "low";
            }
            Err(e) => {
                println!("{id} #{}: {e}", answers.len() + 1);
                transient |= !model_refused(&e);
                last_error = e.to_string();
                answers.push(None);
            }
        }
        if answers.len() == EARLY_CHECK {
            let ms: Vec<u64> = answers.iter().flatten().map(|a| a.ms).collect();
            note = match ranking::spread(&ms).0 {
                None => format!("niedostępny: {}", last_error.chars().take(160).collect::<String>()),
                Some(m) if m > TOO_SLOW_MS => {
                    let s = format!("{:.1}", m as f64 / 1000.0).replace('.', ",");
                    format!("przerwany: mediana pierwszych {EARLY_CHECK} dyktowań {s} s")
                }
                Some(_) => String::new(),
            };
            if !note.is_empty() {
                break;
            }
        }
    }
    let ok = answers.iter().flatten().count();
    println!("{id}: {ok} z {} odpowiedzi, effort {effort} {note}", answers.len());
    Run { id, name, request, effort: effort.into(), answers, note, transient }
}

fn measurement(run: &Run, kind: Kind, prompt: &str, set: &str, at: u64) -> Measurement {
    let ms: Vec<u64> = run.answers.iter().flatten().map(|a| a.ms).collect();
    let costs: Vec<f64> = run.answers.iter().flatten().filter_map(|a| a.cost).collect();
    let (median_ms, p90_ms) = ranking::spread(&ms);
    Measurement {
        at,
        model: run.id.clone(),
        name: run.name.clone(),
        request_model: run.request.clone(),
        effort: run.effort.clone(),
        kind,
        prompt_sha: prompt.into(),
        set_sha: set.into(),
        cases: run.answers.len(),
        errors: run.answers.iter().filter(|a| a.is_none()).count(),
        median_ms,
        p90_ms,
        cost_usd: ranking::median_f64(&costs),
        judge: String::new(),
        score: None,
        good: 0,
        issues: Vec::new(),
        note: run.note.clone(),
    }
}

#[derive(Debug, PartialEq)]
struct Verdict {
    score: f64,
    issues: String,
}

/// The judge's JSON object, possibly wrapped in prose or a code fence.
fn parse_verdicts(reply: &str) -> Option<HashMap<String, Verdict>> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let verdicts: HashMap<String, Verdict> = v
        .as_object()?
        .iter()
        .filter_map(|(label, o)| {
            let score = o.get("score")?.as_f64()?.clamp(0.0, 10.0);
            let issues = o.get("issues").and_then(Value::as_str).unwrap_or_default().trim().to_string();
            Some((label.clone(), Verdict { score, issues }))
        })
        .collect();
    (!verdicts.is_empty()).then_some(verdicts)
}

/// The same outputs in a stable but meaningless order per dictation, so the
/// judge cannot learn which label is which model.
fn shuffled<'a>(case: &str, outputs: Vec<(&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
    let mut v = outputs;
    v.sort_by_cached_key(|(id, _)| prompt_sha(&format!("{case}\n{id}")));
    v
}

async fn ask_judge(client: &Client, key: &str, user: &str) -> Result<String, String> {
    let body = json!({
        "model": JUDGE_MODEL,
        "max_tokens": 4000,
        "messages": [{"role": "system", "content": JUDGE_SYSTEM}, {"role": "user", "content": user}],
    });
    let resp = client
        .post(chat_url(&judge_base()))
        .bearer_auth(key)
        .json(&body)
        .timeout(JUDGE_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    v.pointer("/choices/0/message/content").and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("brak treści: {v}"))
}

/// Scores per run and dictation; None where the judge gave none.
async fn judge_chunk(
    client: &Client,
    key: &str,
    cases: &[Case],
    chunk: &[Run],
    anchors: &[(String, HashMap<String, String>)],
) -> Vec<Vec<Option<Verdict>>> {
    let jobs = cases.iter().enumerate().map(|(i, case)| async move {
        let mut outputs: Vec<(&str, &str)> = chunk
            .iter()
            .filter_map(|r| Some((r.id.as_str(), r.answers.get(i)?.as_ref()?.text.as_str())))
            .collect();
        if outputs.is_empty() {
            return (i, HashMap::new());
        }
        outputs.extend(anchors.iter().filter_map(|(id, out)| Some((id.as_str(), out.get(&case.key)?.as_str()))));
        let outputs = shuffled(&case.key, outputs);
        let label = |n: usize| char::from(b'A' + n as u8).to_string();
        let shown: Vec<String> = outputs.iter().enumerate().map(|(n, (_, text))| format!("### {}\n{text}", label(n))).collect();
        let user = format!(
            "<prompt_systemowy>\n{AGENT_PROMPT}\n</prompt_systemowy>\n\n<transkrypcja>\n{}\n</transkrypcja>\n\nWyniki:\n\n{}",
            case.transcript,
            shown.join("\n\n")
        );
        for attempt in 1..=2 {
            match ask_judge(client, key, &user).await.map(|r| parse_verdicts(&r).ok_or(r)) {
                Ok(Ok(mut verdicts)) => {
                    let by_id: HashMap<String, Verdict> = outputs
                        .iter()
                        .enumerate()
                        .filter_map(|(n, (id, _))| Some((id.to_string(), verdicts.remove(&label(n))?)))
                        .collect();
                    return (i, by_id);
                }
                Ok(Err(reply)) => println!("sędzia #{} (próba {attempt}): nie JSON: {}", i + 1, reply.chars().take(200).collect::<String>()),
                Err(e) => println!("sędzia #{} (próba {attempt}): {e}", i + 1),
            }
        }
        (i, HashMap::new())
    });
    let mut by_case: Vec<(usize, HashMap<String, Verdict>)> = stream::iter(jobs).buffer_unordered(PARALLEL_JUDGE).collect().await;
    by_case.sort_by_key(|(i, _)| *i);
    chunk
        .iter()
        .map(|r| by_case.iter_mut().map(|(_, v)| v.remove(&r.id)).collect())
        .collect()
}

/// Fill in the judge's part; None when the judge scored too few dictations
/// to trust (the model is then tested again next run).
fn scored(mut m: Measurement, run: &Run, verdicts: &[Option<Verdict>], cases: &[Case]) -> Option<Measurement> {
    let answered = run.answers.iter().flatten().count();
    let judged = verdicts.iter().flatten().count();
    if judged * 2 < answered {
        return None;
    }
    let mut scores = Vec::new();
    for (i, answer) in run.answers.iter().enumerate() {
        match (answer, verdicts.get(i).and_then(Option::as_ref)) {
            (None, _) => scores.push(0.0),
            (Some(_), Some(v)) => {
                scores.push(v.score);
                if v.score < 8.0 {
                    let start: String = cases[i].transcript.chars().take(30).collect();
                    m.issues.push(format!("„{start}…” ({}): {}", v.score, v.issues));
                }
            }
            (Some(_), None) => {}
        }
    }
    m.score = Some((scores.iter().sum::<f64>() / scores.len() as f64 * 10.0).round() / 10.0);
    m.good = scores.iter().filter(|&&s| s >= 9.0).count();
    m.judge = JUDGE_MODEL.into();
    Some(m)
}

/// Stored outputs of the best scored models with this prompt and set, other
/// than the models about to be judged. By score, not by the ranking order, so
/// the judge's scale stays put.
fn anchors(log: &[Measurement], prompt: &str, set: &str, dir: &Path, pending: &[Run]) -> Vec<(String, HashMap<String, String>)> {
    let current: Vec<Measurement> = log.iter().filter(|m| m.set_sha == set).cloned().collect();
    let mut best: Vec<_> = ranking::build(&current, prompt, "", 0, &|_| true, 0)
        .models
        .into_iter()
        .filter(|m| !m.other_prompt && m.score.is_some() && !pending.iter().any(|r| r.id == m.model))
        .collect();
    best.sort_by(|a, b| b.score.unwrap_or(-1.0).total_cmp(&a.score.unwrap_or(-1.0)));
    best.into_iter()
        .filter_map(|m| {
            let text = std::fs::read_to_string(outputs_file(dir, &m.model)).ok()?;
            Some((m.model, serde_json::from_str(&text).ok()?))
        })
        .take(ANCHORS)
        .collect()
}

#[tokio::main]
async fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let mut data = PathBuf::from(&home).join("hyperrust-ranking");
    let mut publish: Option<PathBuf> = None;
    let mut forced: Vec<String> = Vec::new();
    let mut only_plan = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => data = it.next().expect("--data KATALOG").into(),
            "--publish" => publish = Some(it.next().expect("--publish PLIK").into()),
            "--models" => forced = it.next().expect("--models a/b,c/d").split(',').map(|s| s.trim().to_string()).collect(),
            "--plan" => only_plan = true,
            other => panic!("nieznany argument {other}"),
        }
    }
    let corpus = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dictations.txt"));
    let mut texts = parse_entries(&read(corpus));
    texts.extend(parse_history_set(&read(&data.join("set/hist.jsonl"))));
    let set = prompt_sha(&texts.join("\n---\n"));
    let cases: Vec<Case> = texts.into_iter().map(|t| Case { key: prompt_sha(&t), transcript: t }).collect();
    let prompt = prompt_sha(AGENT_PROMPT);
    let log_path = data.join("measurements.jsonl");
    let mut log = load_log(&log_path);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("zegar")
        .as_secs();

    let client = build_client().expect("klient HTTP");
    let models = catalog::get_list(client.get(catalog::OPENROUTER_MODELS_URL)).await.expect("katalog OpenRouter");
    let plan = ranking::plan(&models, &log, &prompt, &forced, now);
    println!("prompt {prompt}, zestaw {set} ({} dyktowań); pełny test: {:?}; ponowny pomiar czasu: {:?}", cases.len(), plan.full, plan.retime);
    if only_plan {
        return;
    }
    let or_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
    let judge_key = std::env::var("CLIPROXY_API_KEY").expect("CLIPROXY_API_KEY");
    let name = |id: &str| {
        let listed = models.iter().find(|m| m.id == id).map(|m| m.name.clone());
        listed.or_else(|| log.iter().rev().find(|m| m.model == id).map(|m| m.name.clone())).unwrap_or_else(|| id.into())
    };
    let jobs: Vec<(String, String, Kind)> = plan
        .full
        .iter()
        .map(|id| (id.clone(), name(id), Kind::Full))
        .chain(plan.retime.iter().map(|id| (id.clone(), name(id), Kind::Speed)))
        .collect();
    let mut runs: Vec<(Run, Kind)> = stream::iter(jobs)
        .map(|(id, name, kind)| {
            let (client, key, cases) = (client.clone(), or_key.clone(), &cases);
            async move { (run_model(client, key, id, name, cases).await, kind) }
        })
        .buffer_unordered(PARALLEL)
        .collect()
        .await;
    // Judge in the plan's order: the first chunk becomes the anchors of the next.
    runs.sort_by_key(|(r, _)| plan.full.iter().chain(&plan.retime).position(|id| *id == r.id));

    let outputs_dir = data.join("outputs").join(format!("{prompt}-{set}"));
    std::fs::create_dir_all(&outputs_dir).unwrap_or_else(|e| panic!("{}: {e}", outputs_dir.display()));
    let mut failed = false;
    let mut pending: Vec<Run> = Vec::new();
    for (run, kind) in runs {
        // Only errors from an outage or the account (network, credits, rate
        // limit) say nothing about the model: not logged, the next run tries
        // again. A model the provider refuses is logged as unavailable.
        if run.answers.iter().all(Option::is_none) && run.transient {
            println!("{}: same błędy, bez zapisu; następny przebieg spróbuje ponownie", run.id);
            continue;
        }
        // Timing runs and tests stopped early have nothing to judge.
        if kind == Kind::Speed || !run.note.is_empty() {
            let m = measurement(&run, kind, &prompt, &set, now);
            append_log(&log_path, &m);
            log.push(m);
        } else {
            pending.push(run);
        }
    }
    while !pending.is_empty() {
        // A model tested again (`--models`) is not its own anchor.
        let anchors = anchors(&log, &prompt, &set, &outputs_dir, &pending);
        let take = PER_JUDGE.saturating_sub(anchors.len()).clamp(1, pending.len());
        let chunk: Vec<Run> = pending.drain(..take).collect();
        let names: Vec<&str> = anchors.iter().map(|(id, _)| id.as_str()).collect();
        println!("sędzia: {} modeli, kotwice {names:?}", chunk.len());
        let verdicts = judge_chunk(&client, &judge_key, &cases, &chunk, &anchors).await;
        for (run, verdicts) in chunk.iter().zip(verdicts) {
            let Some(m) = scored(measurement(run, Kind::Full, &prompt, &set, now), run, &verdicts, &cases) else {
                println!("{}: sędzia ocenił za mało dyktowań, test powtórzy się w następnym przebiegu", run.id);
                failed = true;
                continue;
            };
            let outputs: HashMap<&str, &str> = cases
                .iter()
                .zip(&run.answers)
                .filter_map(|(c, a)| Some((c.key.as_str(), a.as_ref()?.text.as_str())))
                .collect();
            write_atomic(&outputs_file(&outputs_dir, &run.id), &serde_json::to_vec_pretty(&outputs).expect("JSON"));
            append_log(&log_path, &m);
            log.push(m);
        }
    }

    let listed = |id: &str| models.iter().any(|m| m.chat && m.id == id);
    let ranking: Ranking = ranking::build(&log, &prompt, JUDGE_MODEL, cases.len(), &listed, now);
    let body = serde_json::to_vec_pretty(&ranking).expect("JSON");
    write_atomic(&data.join(ranking::FILE_NAME), &body);
    if let Some(path) = &publish {
        write_atomic(path, &body);
    }
    for (n, m) in ranking.models.iter().take(12).enumerate() {
        let score = m.score.map_or("–".into(), |s| format!("{s:.1}"));
        let median = m.median_ms.map_or("–".into(), |ms| format!("{:.2} s", ms as f64 / 1000.0));
        println!("{:2}. {:45} {score:>4}  {median:>7}  {} {}", n + 1, m.use_model, m.effort, m.note);
    }
    if failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_come_out_of_prose_and_fences() {
        let reply = "Oto ocena:\n```json\n{\"A\": {\"score\": 8.5, \"issues\": \" brak przecinka \"}, \"B\": {\"score\": 12}, \"C\": {\"issues\": \"x\"}}\n```";
        let v = parse_verdicts(reply).unwrap();
        assert_eq!(v["A"], Verdict { score: 8.5, issues: "brak przecinka".into() });
        assert_eq!(v["B"].score, 10.0);
        assert!(!v.contains_key("C"));
        assert!(parse_verdicts("nie wiem").is_none());
        assert!(parse_verdicts("{}").is_none());
    }

    #[test]
    fn order_is_stable_per_dictation_and_differs_between_them() {
        let outputs = || vec![("a/one", "1"), ("b/two", "2"), ("c/three", "3"), ("d/four", "4")];
        assert_eq!(shuffled("k1", outputs()), shuffled("k1", outputs()));
        let orders: std::collections::HashSet<Vec<&str>> =
            (0..8).map(|k| shuffled(&format!("k{k}"), outputs()).into_iter().map(|(id, _)| id).collect()).collect();
        assert!(orders.len() > 1);
    }

    fn run(answers: Vec<Option<u64>>) -> Run {
        Run {
            id: "v/m".into(),
            name: "M".into(),
            request: "v/m:nitro".into(),
            effort: "none".into(),
            answers: answers.into_iter().map(|ms| ms.map(|ms| Answer { text: "t".into(), ms, cost: Some(0.0001) })).collect(),
            note: String::new(),
            transient: false,
        }
    }

    #[test]
    fn only_refusals_of_the_model_itself_count_against_it() {
        let http = |status| hyperrust_core::Error::Http { status, message: String::new() };
        assert!(model_refused(&http(404)));
        assert!(model_refused(&http(400)));
        assert!(!model_refused(&http(500)));
        assert!(!model_refused(&hyperrust_core::Error::RateLimited { status: 429, message: String::new() }));
        assert!(!model_refused(&hyperrust_core::Error::Timeout(REQUEST_TIMEOUT)));
    }

    #[test]
    fn failed_requests_score_zero_and_too_few_verdicts_are_not_trusted() {
        let cases: Vec<Case> = (0..4).map(|i| Case { key: i.to_string(), transcript: format!("dyktowanie {i}") }).collect();
        let r = run(vec![Some(400), Some(500), None, Some(600)]);
        let v = |s: f64| Some(Verdict { score: s, issues: if s < 8.0 { "usterka".into() } else { String::new() } });
        let m = scored(measurement(&r, Kind::Full, "p", "s", 1), &r, &[v(9.0), v(7.0), None, v(10.0)], &cases).unwrap();
        // (9 + 7 + 0 + 10) / 4
        assert_eq!((m.score, m.good, m.errors, m.median_ms), (Some(6.5), 2, 1, Some(500)));
        assert_eq!(m.issues, ["„dyktowanie 1…” (7): usterka"]);
        assert!(scored(measurement(&r, Kind::Full, "p", "s", 1), &r, &[v(9.0), None, None, None], &cases).is_none());
    }
}

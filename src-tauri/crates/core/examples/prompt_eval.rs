//! Post-processing quality check: run a system prompt over a set of dictated
//! transcripts and print each result with its latency, to compare prompt
//! versions and models side by side. With `--history` the transcripts come from
//! real dictations (`hyperrust_core::history`, on the history server in
//! `~/hyperrust-history/<device>.jsonl`) and each result is shown next to the
//! text the app pasted at the time.
//!
//! ```text
//! CLIPROXY_API_KEY=… cargo run -p hyperrust-core --example prompt_eval -- \
//!     [--model claude-haiku-5-5] [--prompt-file nowy.txt] [--base URL] [--key-env VAR] \
//!     [--file tests/fixtures/dictations.txt | --history ~/hyperrust-history/PC.jsonl [--last 20]]
//! ```

use std::time::{Duration, Instant};

use hyperrust_core::config::LlmSettings;
use hyperrust_core::http::build_client;
use hyperrust_core::llm::{chat_body, complete_http, effective_effort};
use hyperrust_core::modes::AGENT_PROMPT;
use serde_json::Value;

struct Case {
    transcript: String,
    /// What the app pasted, with the prompt fingerprint and model it used.
    previous: Option<(String, String)>,
}

fn parse_entries(text: &str) -> Vec<String> {
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    body.split("\n---\n")
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect()
}

/// The newest `last` post-processed dictations of a history file, oldest first.
fn parse_history(text: &str, last: usize) -> Vec<Case> {
    let field = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let cases: Vec<Case> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| !field(e, "llm").is_empty() && !field(e, "transcript").trim().is_empty())
        .map(|e| Case {
            transcript: field(&e, "transcript"),
            previous: Some((
                field(&e, "output"),
                format!("{}, {}, prompt {}", field(&e, "outcome"), field(&e, "llm"), field(&e, "prompt_sha")),
            )),
        })
        .collect();
    let start = cases.len().saturating_sub(last);
    cases.into_iter().skip(start).collect()
}

#[tokio::main]
async fn main() {
    let mut base = std::env::var("CLIPROXY_BASE_URL").unwrap_or_else(|_| "http://localhost:8317/v1".into());
    let mut key_env = String::from("CLIPROXY_API_KEY");
    let mut model = LlmSettings::default().model;
    let mut effort = String::new();
    let mut prompt = AGENT_PROMPT.to_string();
    let mut file = String::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dictations.txt"));
    let mut history: Option<String> = None;
    let mut last = 20usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = it.next().expect("--base URL"),
            "--key-env" => key_env = it.next().expect("--key-env VAR"),
            "--model" => model = it.next().expect("--model ID"),
            "--effort" => effort = it.next().expect("--effort LEVEL"),
            "--prompt-file" => {
                let path = it.next().expect("--prompt-file PLIK");
                prompt = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            }
            "--file" => file = it.next().expect("--file PLIK"),
            "--history" => history = Some(it.next().expect("--history PLIK")),
            "--last" => last = it.next().and_then(|n| n.parse().ok()).expect("--last N"),
            other => panic!("nieznany argument {other}"),
        }
    }
    let cases: Vec<Case> = match &history {
        Some(path) => parse_history(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}")), last),
        None => parse_entries(&std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}")))
            .into_iter()
            .map(|transcript| Case { transcript, previous: None })
            .collect(),
    };
    let key = std::env::var(&key_env).ok();
    let client = build_client().expect("klient HTTP");
    let mut totals = Vec::new();

    // Same effort as the app would send for this prompt and model.
    let effort = effective_effort(&effort, &model, &prompt);
    println!(
        "model={model} effort={effort:?} prompt={} wpisów={}",
        hyperrust_core::history::prompt_sha(&prompt),
        cases.len()
    );
    for (i, case) in cases.iter().enumerate() {
        let transcript = &case.transcript;
        let body = chat_body(&model, &prompt, transcript, effort);
        let start = Instant::now();
        let result = complete_http(&client, &base, key.as_deref(), &body, Duration::from_secs(90)).await;
        match &result {
            Ok(out) => {
                let total = start.elapsed().as_millis();
                totals.push(total);
                let ttft = out.first_token.map(|f| f.duration_since(start).as_millis());
                println!("\n### {} ({} ms, TTFT {} ms)", i + 1, total, ttft.map_or("–".into(), |t| t.to_string()));
            }
            Err(e) => println!("\n### {} BŁĄD: {e}", i + 1),
        }
        println!("> {transcript}");
        if let Some((text, info)) = &case.previous {
            println!("\nw aplikacji ({info}):\n{text}\n\nteraz:");
        }
        if let Ok(out) = result {
            println!("\n{}", out.text);
        }
    }
    totals.sort_unstable();
    if let Some(m) = totals.get(totals.len() / 2) {
        println!("\nmediana całości: {m} ms ({} z {})", totals.len(), cases.len());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn splits_entries_and_skips_comments() {
        let e = super::parse_entries("# komentarz\na\nb\n---\n\nc\n---\n");
        assert_eq!(e, ["a\nb", "c"]);
    }

    #[test]
    fn history_keeps_newest_post_processed_dictations() {
        let text = [
            r#"{"transcript":"a","output":"A","llm":"m","outcome":"ok","prompt_sha":"x"}"#,
            r#"{"transcript":"surowa","output":"surowa","llm":"","outcome":"ok"}"#,
            "zepsuta linia",
            r#"{"transcript":"b","output":"B","llm":"m","outcome":"ok","prompt_sha":"x"}"#,
            r#"{"transcript":"c","output":"c","llm":"m","outcome":"llm_error_raw","prompt_sha":"x"}"#,
        ]
        .join("\n");
        let cases = super::parse_history(&text, 2);
        let got: Vec<_> = cases.iter().map(|c| c.transcript.as_str()).collect();
        assert_eq!(got, ["b", "c"]);
        assert_eq!(cases[1].previous.as_ref().unwrap().1, "llm_error_raw, m, prompt x");
    }
}

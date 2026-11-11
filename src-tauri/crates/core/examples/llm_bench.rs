//! Post-processing latency benchmark: time to first token and total time for
//! a set of models on the same transcript.
//!
//! ```text
//! CLIPROXY_API_KEY=… cargo run -p hyperrust-core --example llm_bench -- \
//!     --base http://localhost:8317/v1 --key-env CLIPROXY_API_KEY --runs 3 model-a model-b
//! cargo run -p hyperrust-core --example llm_bench -- --cli claude --runs 2 haiku
//! ```

use std::time::{Duration, Instant};

use hyperrust_core::http::build_client;
use hyperrust_core::llm::{chat_body, complete_cli, complete_http, CliKind};
use hyperrust_core::modes::AGENT_PROMPT;

const TRANSCRIPT: &str = "I popraw w pliku package JSON skrypt build, żeby no uruchamiał najpierw testy, a potem to znaczy nie, najpierw linter, a potem testy. I jeszcze dodaj do readme krótką sekcję o instalacji";

fn median(v: &mut [u128]) -> u128 {
    v.sort_unstable();
    v[v.len() / 2]
}

#[tokio::main]
async fn main() {
    let mut base = std::env::var("CLIPROXY_BASE_URL").unwrap_or_else(|_| "http://localhost:8317/v1".into());
    let mut key_env = String::from("CLIPROXY_API_KEY");
    let mut runs = 3usize;
    let mut effort = String::new();
    let mut cli: Option<CliKind> = None;
    let mut models = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = it.next().expect("--base URL"),
            "--key-env" => key_env = it.next().expect("--key-env VAR"),
            "--runs" => runs = it.next().and_then(|n| n.parse().ok()).expect("--runs N"),
            "--effort" => effort = it.next().expect("--effort LEVEL"),
            "--cli" => {
                cli = Some(match it.next().as_deref() {
                    Some("claude") => CliKind::Claude,
                    Some("codex") => CliKind::Codex,
                    other => panic!("--cli claude|codex, nie {other:?}"),
                })
            }
            m => models.push(m.to_string()),
        }
    }
    if models.is_empty() {
        models.push(String::new());
    }
    let key = std::env::var(&key_env).ok();
    let client = build_client().expect("klient HTTP");
    let timeout = Duration::from_secs(90);

    println!("| model | TTFT mediana [ms] | całość mediana [ms] | całość min–max [ms] | ok/próby |");
    println!("|---|---|---|---|---|");
    for model in &models {
        let (mut ttft, mut total) = (Vec::new(), Vec::new());
        let mut last = String::new();
        let mut errors = Vec::new();
        for _ in 0..runs {
            let start = Instant::now();
            let res = match cli {
                Some(kind) => complete_cli(kind, model, AGENT_PROMPT, TRANSCRIPT, timeout).await,
                None => {
                    let body = chat_body(model, AGENT_PROMPT, TRANSCRIPT, &effort);
                    complete_http(&client, &base, key.as_deref(), &body, timeout).await
                }
            };
            match res {
                Ok(out) => {
                    total.push(start.elapsed().as_millis());
                    if let Some(f) = out.first_token {
                        ttft.push(f.duration_since(start).as_millis());
                    }
                    last = out.text;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        let ok = total.len();
        let name = if model.is_empty() { "(domyślny)" } else { model };
        if ok == 0 {
            println!("| {name} | – | – | – | 0/{runs} |");
        } else {
            let (lo, hi) = (*total.iter().min().unwrap(), *total.iter().max().unwrap());
            let t = if ttft.is_empty() { "–".to_string() } else { median(&mut ttft).to_string() };
            println!("| {name} | {t} | {} | {lo}–{hi} | {ok}/{runs} |", median(&mut total));
        }
        eprintln!("--- {name}: {}", last.replace('\n', " ⏎ "));
        for e in errors {
            eprintln!("--- {name}: błąd: {e}");
        }
    }
}

//! Post-processing through an LLM: any OpenAI-compatible `/chat/completions`
//! endpoint with SSE streaming (CLIProxy, OpenRouter, …) or a local CLI
//! (`claude -p`, `codex exec`) running on the user's subscription.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{Error, Result};
use crate::modes::{extract_output, wrap_transcript, CLEANUP_PROMPT};

/// Request body for `/chat/completions`. Streaming is always on: some upstreams
/// (Antigravity through CLIProxy) reject non-streaming calls, and the first
/// token time is part of the latency log.
pub fn chat_body(model: &str, system: &str, transcript: &str, reasoning_effort: &str) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": wrap_transcript(transcript) },
        ],
    });
    let effort = reasoning_effort.trim();
    if !effort.is_empty() {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

/// The `reasoning_effort` actually sent. With the built-in cleanup prompt Haiku
/// 5.5 often thinks adaptively first (TTFT 2.5–9 s instead of ~1 s), and the
/// cleanup needs no reasoning, so Haiku runs it with thinking off unless the user
/// set an effort. The agent prompt keeps the default: `none` makes Haiku drop or
/// echo content there. Other models are left alone (Sonnet 5.5 rejects `none`).
pub fn effective_effort<'a>(configured: &'a str, model: &str, system: &str) -> &'a str {
    if configured.trim().is_empty() && system.trim() == CLEANUP_PROMPT && model.to_ascii_lowercase().contains("haiku") {
        "none"
    } else {
        configured
    }
}

pub fn chat_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim().trim_end_matches('/'))
}

#[derive(Debug, PartialEq)]
pub enum SseItem {
    Delta(String),
    /// `usage.cost` of the request (OpenRouter sends it with the last chunk).
    Cost(f64),
    /// `choices[0].finish_reason` was set: the generation ended normally.
    Finished,
    Done,
    Error(String),
}

/// Incremental parser for OpenAI-style server-sent events.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
}

impl SseParser {
    pub fn push(&mut self, chunk: &str) -> Vec<SseItem> {
        self.buf.push_str(chunk);
        let mut items = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                items.push(SseItem::Done);
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| err.to_string());
                items.push(SseItem::Error(msg));
                continue;
            }
            if let Some(text) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                if !text.is_empty() {
                    items.push(SseItem::Delta(text.to_string()));
                }
            }
            if let Some(cost) = v.pointer("/usage/cost").and_then(Value::as_f64) {
                items.push(SseItem::Cost(cost));
            }
            if v.pointer("/choices/0/finish_reason").and_then(Value::as_str).is_some() {
                items.push(SseItem::Finished);
            }
        }
        items
    }
}

/// Result of a post-processing call.
#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub text: String,
    pub first_token: Option<Instant>,
    /// USD the endpoint billed, when it says (OpenRouter); None for subscriptions.
    pub cost_usd: Option<f64>,
}

/// Call an OpenAI-compatible endpoint and return the extracted result text.
pub async fn complete_http(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<LlmOutput> {
    let call = async {
        let mut rb = client.post(chat_url(base_url)).json(body);
        if let Some(k) = api_key.filter(|k| !k.trim().is_empty()) {
            rb = rb.bearer_auth(k.trim());
        }
        let resp = rb.send().await.map_err(Error::from)?;
        let status = resp.status().as_u16();
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::from_status(status, &text));
        }
        if is_json {
            // The server ignored `stream: true`.
            let v: Value = resp.json().await.map_err(Error::from)?;
            let text = v
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidResponse("brak choices[0].message.content".into()))?;
            let cost_usd = v.pointer("/usage/cost").and_then(Value::as_f64);
            return Ok(LlmOutput { text: extract_output(text), first_token: Some(Instant::now()), cost_usd });
        }
        let mut parser = SseParser::default();
        let mut raw = String::new();
        let mut first_token = None;
        let mut cost_usd = None;
        let mut stream = resp.bytes_stream();
        let mut pending: Vec<u8> = Vec::new();
        let mut finished = false;
        'outer: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Error::from)?;
            pending.extend_from_slice(&chunk);
            // Only hand complete UTF-8 to the parser.
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            let text = String::from_utf8_lossy(&pending[..valid]).to_string();
            pending.drain(..valid);
            for item in parser.push(&text) {
                match item {
                    SseItem::Delta(d) => {
                        first_token.get_or_insert_with(Instant::now);
                        raw.push_str(&d);
                    }
                    SseItem::Cost(c) => cost_usd = Some(c),
                    SseItem::Finished => finished = true,
                    SseItem::Done => {
                        finished = true;
                        break 'outer;
                    }
                    SseItem::Error(e) => return Err(Error::Provider(e)),
                }
            }
        }
        // A stream cut off mid-answer would paste a truncated prompt; the
        // caller falls back to the full raw transcript instead.
        if !finished {
            return Err(Error::InvalidResponse("strumień urwał się przed końcem odpowiedzi".into()));
        }
        if raw.trim().is_empty() {
            return Err(Error::InvalidResponse("model zwrócił pustą odpowiedź".into()));
        }
        Ok(LlmOutput { text: extract_output(&raw), first_token, cost_usd })
    };
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| Error::Timeout(timeout))?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    Claude,
    Codex,
}

impl CliKind {
    pub fn program(self) -> &'static str {
        match self {
            CliKind::Claude => "claude",
            CliKind::Codex => "codex",
        }
    }
}

/// Command line and stdin for a CLI backend. The transcript always travels on
/// stdin; for `claude` the system prompt goes in `--system-prompt` unless the
/// binary is a `.cmd` shim, whose argument parsing cannot carry newlines safely.
pub fn cli_invocation(kind: CliKind, model: &str, system: &str, transcript: &str, via_cmd_shim: bool) -> (Vec<String>, String) {
    let user = wrap_transcript(transcript);
    let model = model.trim();
    match kind {
        CliKind::Claude => {
            // Not `--bare`: it reads only ANTHROPIC_API_KEY and ignores the OAuth
            // (subscription) login. These flags skip the same startup work.
            let mut args: Vec<String> = [
                "-p",
                "--output-format",
                "text",
                "--no-session-persistence",
                "--tools",
                "",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            if !model.is_empty() {
                args.push("--model".into());
                args.push(model.into());
            }
            if via_cmd_shim {
                (args, format!("{system}\n\n{user}"))
            } else {
                args.push("--system-prompt".into());
                args.push(system.into());
                (args, user)
            }
        }
        CliKind::Codex => {
            let mut args: Vec<String> = ["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            if !model.is_empty() {
                args.push("-m".into());
                args.push(model.into());
            }
            args.push("-".into());
            (args, format!("{system}\n\n{user}"))
        }
    }
}

/// Locate a CLI on PATH, including the `.exe`/`.cmd` variants on Windows.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Run a CLI backend and return the extracted result.
pub async fn complete_cli(kind: CliKind, model: &str, system: &str, transcript: &str, timeout: Duration) -> Result<LlmOutput> {
    let program = find_program(kind.program())
        .ok_or_else(|| Error::Other(format!("nie znaleziono `{}` w PATH", kind.program())))?;
    let ext = program
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let shim = ext == "cmd" || ext == "bat";
    let (args, stdin_text) = cli_invocation(kind, model, system, transcript, shim);

    let mut cmd = if shim {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(&program).args(&args);
        c
    } else {
        let mut c = tokio::process::Command::new(&program);
        c.args(&args);
        c
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let run = async {
        let mut child = cmd.spawn().map_err(|e| Error::Other(format!("{}: {e}", kind.program())))?;
        // Drain stderr concurrently: a chatty CLI must not block on a full pipe.
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut err = Vec::new();
                let _ = stderr.read_to_end(&mut err).await;
                String::from_utf8_lossy(&err).to_string()
            })
        });
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_text.as_bytes())
                .await
                .map_err(|e| Error::Other(e.to_string()))?;
        }
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        let mut first_token = None;
        loop {
            let n = stdout.read(&mut buf).await.map_err(|e| Error::Other(e.to_string()))?;
            if n == 0 {
                break;
            }
            first_token.get_or_insert_with(Instant::now);
            out.extend_from_slice(&buf[..n]);
        }
        let status = child.wait().await.map_err(|e| Error::Other(e.to_string()))?;
        if !status.success() {
            let err = match stderr_task {
                Some(t) => t.await.unwrap_or_default(),
                None => String::new(),
            };
            let tail: String = err.trim().chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(Error::Other(format!("{} zakończył się kodem {status}: {tail}", kind.program())));
        }
        let text = String::from_utf8_lossy(&out).to_string();
        if text.trim().is_empty() {
            return Err(Error::InvalidResponse(format!("{} nic nie zwrócił", kind.program())));
        }
        Ok(LlmOutput { text: extract_output(&text), first_token, cost_usd: None })
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| Error::Timeout(timeout))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_streams_and_wraps_transcript() {
        let b = chat_body("m", "SYS", " zrób to ", "");
        assert_eq!(b["stream"], true);
        assert_eq!(b["messages"][0]["content"], "SYS");
        assert_eq!(b["messages"][1]["content"], "<transkrypcja>\nzrób to\n</transkrypcja>");
        assert!(b.get("reasoning_effort").is_none());
        assert_eq!(chat_body("m", "s", "t", "low")["reasoning_effort"], "low");
        assert_eq!(chat_url("http://h:1/v1/"), "http://h:1/v1/chat/completions");
    }

    #[test]
    fn cleanup_runs_haiku_without_thinking() {
        use crate::modes::AGENT_PROMPT;
        assert_eq!(effective_effort("", "claude-haiku-5-5", CLEANUP_PROMPT), "none");
        assert_eq!(effective_effort(" ", "anthropic/claude-haiku-5.5", CLEANUP_PROMPT), "none");
        assert_eq!(effective_effort("low", "claude-haiku-5-5", CLEANUP_PROMPT), "low");
        assert_eq!(effective_effort("", "claude-haiku-5-5", AGENT_PROMPT), "");
        assert_eq!(effective_effort("", "claude-haiku-5-5", "własny prompt"), "");
        assert_eq!(effective_effort("", "claude-sonnet-5-5", CLEANUP_PROMPT), "");
    }

    #[test]
    fn sse_parser_handles_split_lines_errors_and_done() {
        let mut p = SseParser::default();
        let mut items = p.push("data: {\"choices\":[{\"delta\":{\"content\":\"Na\"}}]}\n\ndata: {\"choi");
        items.extend(p.push("ces\":[{\"delta\":{\"content\":\"praw\"}}]}\r\n: keep-alive\n"));
        items.extend(p.push("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hm\"}}]}\n"));
        items.extend(p.push("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n"));
        items.extend(p.push("data: [DONE]\n"));
        assert_eq!(
            items,
            vec![
                SseItem::Delta("Na".into()),
                SseItem::Delta("praw".into()),
                SseItem::Finished,
                SseItem::Done
            ]
        );
        assert!(p.push("data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n")
            .iter()
            .all(|i| *i != SseItem::Finished));
        assert_eq!(
            SseParser::default().push("data: {\"error\":{\"message\":\"quota\"}}\n"),
            vec![SseItem::Error("quota".into())]
        );
    }

    #[test]
    fn sse_parser_reads_openrouter_cost_from_last_chunk() {
        // Shape of OpenRouter's final chunk (15.10.2026): finish_reason and usage together.
        let last = r#"data: {"provider":"Together","choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":2,"cost":0.0000048}}"#;
        assert_eq!(SseParser::default().push(&format!("{last}\n")), vec![SseItem::Cost(0.0000048), SseItem::Finished]);
    }

    #[test]
    fn replays_recorded_cliproxy_stream() {
        let mut p = SseParser::default();
        let mut raw = String::new();
        let mut done = false;
        for item in p.push(include_str!("../tests/fixtures/cliproxy_stream.sse")) {
            match item {
                SseItem::Delta(d) => raw.push_str(&d),
                SseItem::Finished | SseItem::Cost(_) => {}
                SseItem::Done => done = true,
                SseItem::Error(e) => panic!("{e}"),
            }
        }
        assert!(done);
        let out = extract_output(&raw);
        assert!(!out.is_empty() && !out.contains("<wynik>"), "{out}");
    }

    #[test]
    fn cli_invocations() {
        let (args, stdin) = cli_invocation(CliKind::Claude, "haiku", "SYS", "tekst", false);
        assert_eq!(args[0], "-p");
        assert!(!args.iter().any(|a| a == "--bare"), "--bare wyłącza logowanie subskrypcją");
        assert!(args.windows(2).any(|w| w == ["--setting-sources", ""]));
        assert!(args.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(args.windows(2).any(|w| w == ["--system-prompt", "SYS"]));
        assert_eq!(stdin, "<transkrypcja>\ntekst\n</transkrypcja>");

        let (args, stdin) = cli_invocation(CliKind::Claude, "", "SYS", "tekst", true);
        assert!(!args.iter().any(|a| a == "--system-prompt" || a == "--model"));
        assert!(stdin.starts_with("SYS\n\n<transkrypcja>"));

        let (args, stdin) = cli_invocation(CliKind::Codex, "gpt-5.5", "SYS", "tekst", false);
        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert!(args.windows(2).any(|w| w == ["-m", "gpt-5.5"]));
        assert!(stdin.starts_with("SYS\n\n"));
    }
}

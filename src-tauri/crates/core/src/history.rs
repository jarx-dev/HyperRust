//! Dictation history: what the STT model returned and what post-processing made
//! of it. Kept in a local JSON-lines file and optionally sent to your own server
//! (`scripts/history-server.py`), where agents compare the two and replay real
//! dictations through a new prompt (`examples/prompt_eval.rs --history`).

use std::io;
use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::timing::TimingReport;

/// History server a new installation uploads to, baked in at build time
/// (`HYPERRUST_DEFAULT_HISTORY_URL`); empty = history stays on this computer.
pub const DEFAULT_HISTORY_URL: &str = match option_env!("HYPERRUST_DEFAULT_HISTORY_URL") {
    Some(url) => url,
    None => "",
};
/// Entries kept in the local file.
pub const KEEP_LOCAL: usize = 300;
/// Newest entries sent with each upload. The server stores every id once, so a
/// dictation made while the server was unreachable arrives with a later one.
pub const UPLOAD_BATCH: usize = 20;

/// One dictation. Timings and the mode, provider and outcome labels come
/// flattened from the latency log row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HistoryEntry {
    /// `<device>-<ts_ms>`, unique per dictation.
    pub id: String,
    /// Unix time in milliseconds.
    pub ts_ms: u64,
    pub device: String,
    /// App build that made the dictation.
    pub build: String,
    pub stt_model: String,
    /// [`prompt_sha`] of the system prompt; empty without post-processing.
    pub prompt_sha: String,
    /// Raw STT result.
    pub transcript: String,
    /// The text that was pasted (the raw transcript when post-processing failed).
    pub output: String,
    #[serde(flatten)]
    pub timing: TimingReport,
}

impl HistoryEntry {
    pub fn id_for(device: &str, ts_ms: u64) -> String {
        format!("{device}-{ts_ms}")
    }
}

/// Short fingerprint of a system prompt, to tell which prompt version produced
/// an output.
pub fn prompt_sha(prompt: &str) -> String {
    let digest = Sha256::digest(prompt.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Append `entry` to the JSON-lines file at `path`, keeping the newest `keep`
/// lines. The file is replaced atomically, so a crash never leaves half a file.
pub fn append(path: &Path, entry: &HistoryEntry, keep: usize) -> io::Result<()> {
    let line = serde_json::to_string(entry)?;
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
    lines.push(&line);
    let start = lines.len().saturating_sub(keep);
    let mut out = lines[start..].join("\n");
    out.push('\n');
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)
}

/// The newest `n` entries of the file, oldest first; unreadable lines are skipped.
pub fn recent(path: &Path, n: usize) -> io::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let start = entries.len().saturating_sub(n);
    Ok(entries[start..].to_vec())
}

/// `POST` entries as a JSON array to the history server. Only `https://` and
/// Tailscale/loopback `http://` are accepted (see
/// [`crate::update::validate_private_url`]); use a client without redirects.
pub async fn upload(client: &Client, url: &str, entries: &[Value]) -> Result<()> {
    let url = crate::update::validate_private_url(url, &crate::tr!("history", "historii"))?;
    let resp = client
        .post(url)
        .json(entries)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(Error::from)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::from_status(status, &body));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::{Timeline, TimingMeta};

    fn entry(n: u64) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntry::id_for("PC", n),
            ts_ms: n,
            device: "PC".into(),
            build: "abc".into(),
            stt_model: "scribe_v2_realtime".into(),
            prompt_sha: prompt_sha("p"),
            transcript: format!("surowe {n}"),
            output: format!("wynik {n}"),
            timing: Timeline::start().report(TimingMeta { mode: "agent".into(), outcome: "ok".into(), ..Default::default() }),
        }
    }

    #[test]
    fn keeps_newest_entries_with_flat_timing_fields() {
        let dir = std::env::temp_dir().join(format!("hyperrust-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        for n in 1..=5 {
            append(&path, &entry(n), 3).unwrap();
        }
        let all = recent(&path, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["id"], "PC-3");
        assert_eq!(all[2]["transcript"], "surowe 5");
        assert_eq!(all[2]["outcome"], "ok");
        assert_eq!(all[2]["mode"], "agent");
        let last = recent(&path, 1).unwrap();
        assert_eq!(last[0]["output"], "wynik 5");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prompt_sha_is_short_and_stable() {
        assert_eq!(prompt_sha("abc"), "ba7816bf8f01");
        assert_ne!(prompt_sha("abc"), prompt_sha("abd"));
    }

    #[tokio::test]
    async fn upload_rejects_public_http() {
        let client = crate::update::client().unwrap();
        let err = upload(&client, "http://example.com/history", &[]).await.unwrap_err();
        assert!(err.to_string().contains("history address"), "{err}");
    }
}

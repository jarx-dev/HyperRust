//! Dictation history on this machine: each dictation is appended to
//! `history.jsonl` in the logs folder and the newest entries are sent to the
//! history server, all off the dictation path. The upload waits for
//! a quiet moment, so it never competes with a dictation for the network.
//! Dictated text never reaches the log.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hyperrust_core::config::HistorySettings;
use hyperrust_core::history::{self, HistoryEntry, KEEP_LOCAL, UPLOAD_BATCH};
use tauri::{AppHandle, Manager};

use crate::paths;
use crate::state::AppState;

/// Serializes read-modify-write of the history file between dictations.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The upload starts only after no dictation has run for this long;
/// dictations in a row end in a single upload.
const UPLOAD_QUIET_MS: u64 = 3_000;

/// An upload is waiting for a quiet moment; it will carry newer entries too.
static UPLOAD_PENDING: AtomicBool = AtomicBool::new(false);

/// Name of this computer: the device id on the history server.
pub fn device() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "nieznany".into())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Client without redirects, like the updater's: an allowed host must not be
/// able to forward dictations elsewhere.
fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT.get_or_init(|| hyperrust_core::update::client().ok()).as_ref()
}

/// Every entry of the local history file, newest first; none when it does not exist yet.
pub fn local() -> Result<Vec<serde_json::Value>, String> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match history::recent(&paths::history_file(), KEEP_LOCAL) {
        Ok(mut entries) => {
            entries.reverse();
            Ok(entries)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("odczyt historii: {e}")),
    }
}

/// Save `entry` right away (a few milliseconds on a worker thread) and upload
/// the newest entries once dictation has been quiet for a while.
pub fn record(app: AppHandle, settings: HistorySettings, entry: HistoryEntry) {
    if !settings.enabled {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let path = paths::history_file();
        let file = path.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            history::append(&file, &entry, KEEP_LOCAL)
        })
        .await;
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("historia: zapis lokalny: {e}");
                return;
            }
            Err(e) => {
                tracing::warn!("historia: {e}");
                return;
            }
        }
        let (Some(url), Some(client)) = (settings.upload_target(), client()) else {
            return;
        };
        if UPLOAD_PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        while !app.state::<AppState>().is_idle(UPLOAD_QUIET_MS) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // Cleared before reading the file: an entry saved after this point
        // schedules its own upload.
        UPLOAD_PENDING.store(false, Ordering::SeqCst);
        let batch = tokio::task::spawn_blocking(move || {
            let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            history::recent(&path, UPLOAD_BATCH)
        })
        .await;
        let batch = match batch {
            Ok(Ok(batch)) => batch,
            Ok(Err(e)) => {
                tracing::warn!("historia: odczyt: {e}");
                return;
            }
            Err(e) => {
                tracing::warn!("historia: {e}");
                return;
            }
        };
        if let Err(e) = history::upload(client, url, &batch).await {
            tracing::info!("historia: wysyłka na {url}: {e}");
        }
    });
}

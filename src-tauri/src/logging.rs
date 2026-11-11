//! File logging (plus stdout in debug builds) and the per-dictation timing log.
//! Transcripts and keys are never logged; only lengths and stage timings.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use hyperrust_core::timing::TimingReport;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::paths;

/// Rotate a log file to `<name>.1` once it exceeds this size.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

fn rotate(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
}

fn open_append(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Best effort: logging problems never stop the app.
pub fn init() {
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("hyperrust.log");
    rotate(&path);
    let filter = || EnvFilter::try_from_env("HYPERRUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = open_append(&path).map(|f| {
        fmt::layer()
            .with_writer(Mutex::new(f))
            .with_ansi(false)
            .with_target(false)
            .with_filter(filter())
    });
    let stdout_layer = cfg!(debug_assertions).then(|| fmt::layer().with_target(false).with_filter(filter()));
    let _ = tracing_subscriber::registry().with(file_layer).with(stdout_layer).try_init();
}

/// Append one dictation's timings to `timings.jsonl` and the main log.
pub fn record_timing(report: &TimingReport) {
    tracing::info!("{report}");
    let path = paths::timings_file();
    rotate(&path);
    if let (Some(mut f), Ok(line)) = (open_append(&path), serde_json::to_string(report)) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{{\"ts\":{ts},{}", &line[1..]);
    }
}

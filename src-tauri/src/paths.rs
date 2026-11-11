//! Where HyperRust keeps its files: settings in `%APPDATA%\HyperRust`, logs in
//! `%LOCALAPPDATA%\HyperRust\logs` (XDG directories elsewhere).

use std::path::PathBuf;

const APP_DIR: &str = "HyperRust";

fn base(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))
        .unwrap_or_else(std::env::temp_dir)
}

pub fn config_dir() -> PathBuf {
    if cfg!(windows) {
        base("APPDATA", "AppData/Roaming").join(APP_DIR)
    } else {
        base("XDG_CONFIG_HOME", ".config").join(APP_DIR)
    }
}

pub fn logs_dir() -> PathBuf {
    if cfg!(windows) {
        base("LOCALAPPDATA", "AppData/Local").join(APP_DIR).join("logs")
    } else {
        base("XDG_STATE_HOME", ".local/state").join(APP_DIR).join("logs")
    }
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn timings_file() -> PathBuf {
    logs_dir().join("timings.jsonl")
}

/// Recent dictations with their transcripts (see `hyperrust_core::history`).
pub fn history_file() -> PathBuf {
    logs_dir().join("history.jsonl")
}

//! Shared application state.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use hyperrust_core::timing::TimingReport;
use hyperrust_core::update::BuildInfo;
use hyperrust_core::Settings;
use tokio::sync::mpsc;

use crate::controller::CtlMsg;
use crate::recorder::Recorder;
use crate::secrets::Secrets;
use crate::updater::UpdateHandle;

pub const BUILD: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    build: env!("HYPERRUST_BUILD"),
    built_at: parse_u64(env!("HYPERRUST_BUILT_AT")),
};

const fn parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut n = 0u64;
    while i < bytes.len() {
        n = n * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    n
}

/// The version people see: the build time in local time ("v26.1111.1215":
/// year, month and day, hour and minute), so every update shows a higher
/// number. A build that never updates itself says so.
pub fn version_label() -> String {
    let short = short_version();
    if is_local_build() {
        hyperrust_core::tr!("{short} local", "{short} lokalna")
    } else {
        short
    }
}

/// The build time as `v26.1111.1215`.
pub fn short_version() -> String {
    chrono::DateTime::from_timestamp(BUILD.built_at as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("v%y.%m%d.%H%M").to_string())
        .unwrap_or_default()
}

/// A build that never updates itself (made without `HYPERRUST_BUILD`).
pub fn is_local_build() -> bool {
    env!("HYPERRUST_SELF_UPDATE").is_empty()
}


const RECENT_TIMINGS: usize = 30;

/// Dictations in flight versus an update being installed. A dictation and the
/// EXE swap exclude each other: each side announces itself first, then checks
/// the other (SeqCst on both), so at most one of them proceeds.
struct Activity {
    /// Dictations between key press and the end of paste.
    busy: AtomicUsize,
    installing: AtomicBool,
    last: Mutex<Instant>,
}

impl Activity {
    fn touch(&self) {
        *self.last.lock().expect("activity lock") = Instant::now();
    }
}

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub secrets: Secrets,
    pub http: reqwest::Client,
    pub recorder: Recorder,
    pub ctl: mpsc::UnboundedSender<CtlMsg>,
    pub update: UpdateHandle,
    activity: Arc<Activity>,
    timings: Mutex<VecDeque<TimingReport>>,
}

impl AppState {
    pub fn new(
        settings: Settings,
        http: reqwest::Client,
        recorder: Recorder,
        ctl: mpsc::UnboundedSender<CtlMsg>,
        update: UpdateHandle,
    ) -> Self {
        Self {
            settings: RwLock::new(settings),
            secrets: Secrets::default(),
            http,
            recorder,
            ctl,
            update,
            activity: Arc::new(Activity {
                busy: AtomicUsize::new(0),
                installing: AtomicBool::new(false),
                last: Mutex::new(Instant::now()),
            }),
            timings: Mutex::new(VecDeque::new()),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().expect("settings lock").clone()
    }

    /// Mark a dictation as started; `None` while an update is being installed.
    pub fn try_begin_dictation(&self) -> Option<BusyGuard> {
        let a = &self.activity;
        a.busy.fetch_add(1, Ordering::SeqCst);
        if a.installing.load(Ordering::SeqCst) {
            a.busy.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(BusyGuard(a.clone()))
    }

    /// Reserve the app for an EXE swap if no dictation ran for `quiet_ms`.
    pub fn try_begin_install(&self, quiet_ms: u64) -> bool {
        let a = &self.activity;
        if !self.is_idle(quiet_ms) {
            return false;
        }
        a.installing.store(true, Ordering::SeqCst);
        if a.busy.load(Ordering::SeqCst) != 0 {
            a.installing.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// The swap failed; dictations may start again.
    pub fn end_install(&self) {
        self.activity.installing.store(false, Ordering::SeqCst);
    }

    /// No dictation in flight and none in the last `quiet_ms`.
    pub fn is_idle(&self, quiet_ms: u64) -> bool {
        let a = &self.activity;
        a.busy.load(Ordering::SeqCst) == 0
            && a.last.lock().expect("activity lock").elapsed().as_millis() as u64 >= quiet_ms
    }

    pub fn push_timing(&self, report: TimingReport) {
        let mut t = self.timings.lock().expect("timings lock");
        if t.len() == RECENT_TIMINGS {
            t.pop_front();
        }
        t.push_back(report);
    }

    pub fn recent_timings(&self) -> Vec<TimingReport> {
        self.timings.lock().expect("timings lock").iter().rev().cloned().collect()
    }
}

/// Marks a dictation as in flight; dropping it (also when the task is
/// aborted) marks it finished.
pub struct BusyGuard(Arc<Activity>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.touch();
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_short_and_from_the_build_date() {
        let v = super::version_label();
        let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
        assert!(v.starts_with('v') && digits.len() == 10, "{v}");
    }
}

//! Provider balances for the overview page and the tray tooltip. Fetched at
//! start-up, when the pointer enters the tray icon (at most once a minute),
//! after a key or provider change and whenever the overview page asks; never
//! on a timer. Only providers the settings use are asked and shown.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hyperrust_core::balance::{self, ElevenLabsBalance, OpenRouterBalance};
use hyperrust_core::config::{SecretKey, Settings};
use hyperrust_core::tr;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// A hover refreshes the tooltip only when its lines are older than this.
const STALE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum Source {
    ElevenLabs,
    OpenRouter,
}

impl Source {
    fn used(self, s: &Settings) -> bool {
        match self {
            Source::ElevenLabs => s.uses_elevenlabs(),
            Source::OpenRouter => s.uses_openrouter(),
        }
    }
}

/// Tooltip lines (with their provider) and when they were fetched.
type Lines = (Option<Instant>, Vec<(Source, String)>);
static LINES: Mutex<Lines> = Mutex::new((None, Vec::new()));
static FETCHING: AtomicBool = AtomicBool::new(false);

/// Result of one balance request.
#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Fetched<T> {
    Ok { data: T },
    NoKey,
    /// The settings do not use this provider, so nothing was asked.
    Unused,
    Error { message: String },
}

async fn fetched<T, F>(used: bool, key: Option<String>, request: impl FnOnce(String) -> F) -> Fetched<T>
where
    F: Future<Output = hyperrust_core::Result<T>>,
{
    if !used {
        return Fetched::Unused;
    }
    match key {
        None => Fetched::NoKey,
        Some(key) => match request(key).await {
            Ok(data) => Fetched::Ok { data },
            Err(e) => Fetched::Error { message: e.to_string() },
        },
    }
}

/// Fetch the balances of the providers in use and put them in the tray tooltip:
/// the main ElevenLabs account, the backup one and OpenRouter.
pub async fn fetch(
    app: &AppHandle,
    now_unix: u64,
) -> (Fetched<ElevenLabsBalance>, Fetched<ElevenLabsBalance>, Fetched<OpenRouterBalance>) {
    let state = app.state::<AppState>();
    let http = &state.http;
    let settings = state.settings();
    let elevenlabs_key = |key: SecretKey| {
        fetched(settings.uses_elevenlabs(), state.secrets.get(key), move |key| async move {
            balance::elevenlabs(http, &key, now_unix).await
        })
    };
    let (elevenlabs, elevenlabs_backup, openrouter) = tokio::join!(
        elevenlabs_key(SecretKey::ElevenLabs),
        elevenlabs_key(SecretKey::ElevenLabsBackup),
        fetched(settings.uses_openrouter(), state.secrets.get(SecretKey::OpenRouter), |key| async move {
            balance::openrouter(http, &key).await
        }),
    );

    let credits_per_min = if settings.stt.streaming {
        balance::SCRIBE_REALTIME_CREDITS_PER_MIN
    } else {
        balance::SCRIBE_BATCH_CREDITS_PER_MIN
    };
    let mut lines = Vec::new();
    match (&elevenlabs, &elevenlabs_backup) {
        (Fetched::Ok { data }, Fetched::Ok { data: backup }) => {
            lines.push((Source::ElevenLabs, balance::elevenlabs_pair_line(data, backup, credits_per_min)));
        }
        (Fetched::Ok { data }, _) | (Fetched::NoKey, Fetched::Ok { data }) => {
            lines.push((Source::ElevenLabs, balance::elevenlabs_line(data, credits_per_min, now_unix)));
        }
        (Fetched::Error { message }, _) => {
            tracing::warn!("saldo ElevenLabs: {message}");
            lines.push((Source::ElevenLabs, tr!("ElevenLabs: could not check the balance", "ElevenLabs: nie udało się sprawdzić salda")));
        }
        _ => {}
    }
    if let Fetched::Error { message } = &elevenlabs_backup {
        tracing::warn!("saldo zapasowego konta ElevenLabs: {message}");
    }
    match &openrouter {
        Fetched::Ok { data } => lines.push((Source::OpenRouter, balance::openrouter_line(data))),
        Fetched::Error { message } => {
            tracing::warn!("saldo OpenRouter: {message}");
            lines.push((Source::OpenRouter, tr!("OpenRouter: could not check the balance", "OpenRouter: nie udało się sprawdzić salda")));
        }
        Fetched::NoKey | Fetched::Unused => {}
    }
    // After a failure (e.g. no network right after login) the next hover retries.
    let failed = [&elevenlabs, &elevenlabs_backup].iter().any(|f| matches!(f, Fetched::Error { .. }))
        || matches!(openrouter, Fetched::Error { .. });
    *LINES.lock().expect("balance lines") = ((!failed).then(Instant::now), lines);
    crate::tray::refresh_tooltip(app);
    (elevenlabs, elevenlabs_backup, openrouter)
}

/// The last fetched tooltip lines of the providers `settings` use; a provider
/// dropped from the settings disappears before the next fetch.
pub fn lines(settings: &Settings) -> Vec<String> {
    LINES
        .lock()
        .expect("balance lines")
        .1
        .iter()
        .filter(|(source, _)| source.used(settings))
        .map(|(_, line)| line.clone())
        .collect()
}

/// Whether `before` → `after` starts using a provider with a balance.
pub fn needs_fetch(before: &Settings, after: &Settings) -> bool {
    (after.uses_elevenlabs() && !before.uses_elevenlabs()) || (after.uses_openrouter() && !before.uses_openrouter())
}

/// Fetch in the background unless the lines are fresh (`force` skips that
/// check) or a fetch is already running.
pub fn refresh(app: &AppHandle, force: bool) {
    let fresh = LINES.lock().expect("balance lines").0.is_some_and(|at| at.elapsed() < STALE);
    if (fresh && !force) || FETCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        fetch(&app, crate::history::now_ms() / 1000).await;
        FETCHING.store(false, Ordering::Release);
    });
}

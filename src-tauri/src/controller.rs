//! Dictation controller: turns hotkey events into recordings (via the
//! push-to-talk machine from core) and runs each recording through
//! STT → optional post-processing → paste, logging stage timings.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyperrust_core::config::{HotkeyMode, SecretKey};
use hyperrust_core::cue::Cue;
use hyperrust_core::error::Error;
use hyperrust_core::history::{prompt_sha, HistoryEntry};
use hyperrust_core::hotkey::{HotkeySpec, KeyEvent, PttAction, PttMachine, PttTiming};
use hyperrust_core::session::{llm_label, post_process, stt_cost, transcribe};
use hyperrust_core::stt::SttProviderKind;
use hyperrust_core::timing::{Timeline, TimingMeta};
use hyperrust_core::tr;
use hyperrust_core::audio::TARGET_RATE;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tauri::async_runtime::JoinHandle;

use crate::history;
use crate::hotkey::{self, HookEvent};
use crate::overlay::{self, OverlayState};
use crate::paste;
use crate::sound;
use crate::state::{AppState, BUILD};

pub enum CtlMsg {
    Key(HookEvent),
    /// Settings changed: re-read the shortcut and hotkey mode.
    Reconfigure,
    /// The microphone stream died during a recording.
    MicFailed(String),
    /// The recording indicator was clicked: finish the dictation.
    Finish,
}

/// Paste operations must not interleave (they share the clipboard).
static PASTE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The STT key that last took over from a failing one. Dictations try it
/// first, so a used-up account is skipped until the app restarts or a key changes.
static PREFERRED_STT_KEY: Mutex<Option<SecretKey>> = Mutex::new(None);

/// A stored key changed: start from the provider's own order again.
pub fn reset_preferred_stt_key() {
    *PREFERRED_STT_KEY.lock().expect("preferred STT key") = None;
}

/// The start cue waits this long, so a press cancelled right away (Ctrl+C with
/// a bare-modifier shortcut, Escape) stays silent.
const START_CUE_DELAY: Duration = Duration::from_millis(150);

struct Recording {
    released: Arc<Mutex<Option<Instant>>>,
    pipeline: JoinHandle<()>,
    meter: JoinHandle<()>,
    /// Pending or played start cue (`None` with cues off).
    start_cue: Option<JoinHandle<()>>,
    /// Owner id of the recording indicator.
    overlay_id: u64,
}

pub async fn run(app: AppHandle, mut rx: mpsc::UnboundedReceiver<CtlMsg>) {
    let epoch = Instant::now();
    let ms = |t: Instant| t.saturating_duration_since(epoch).as_millis() as u64;
    let mode = app.state::<AppState>().settings().hotkey_mode;
    let mut ptt = PttMachine::new(mode, PttTiming::default());
    let mut deadline: Option<Instant> = None;
    let mut current: Option<Recording> = None;

    loop {
        let sleep_until = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        let (step, at) = tokio::select! {
            msg = rx.recv() => match msg {
                None => return,
                Some(CtlMsg::Key((event, at))) => {
                    let step = match event {
                        KeyEvent::Pressed => ptt.key_down(ms(at)),
                        KeyEvent::Released => ptt.key_up(ms(at)),
                        KeyEvent::Interference => ptt.interference(),
                        KeyEvent::Escape => ptt.cancel(),
                    };
                    (step, at)
                }
                Some(CtlMsg::Reconfigure) => {
                    let s = app.state::<AppState>().settings();
                    if let Ok(spec) = HotkeySpec::parse(&s.hotkey) {
                        hotkey::set_spec(spec);
                    }
                    if let Some(rec) = current.take() {
                        discard(&app, rec);
                    }
                    ptt.set_mode(s.hotkey_mode);
                    deadline = None;
                    continue;
                }
                Some(CtlMsg::MicFailed(reason)) => {
                    if let Some(rec) = current.take() {
                        discard(&app, rec);
                        ptt.reset();
                        deadline = None;
                        overlay::claim(&app, OverlayState::Error { message: tr!("The microphone stopped working: {reason}", "Mikrofon przestał działać: {reason}") });
                    }
                    hotkey::set_recording(false);
                    continue;
                }
                Some(CtlMsg::Finish) => (ptt.finish(), Instant::now()),
            },
            _ = tokio::time::sleep_until(sleep_until.into()), if deadline.is_some() => {
                let now = Instant::now();
                (ptt.timeout(ms(now)), now)
            }
        };
        deadline = step.deadline.map(|d| epoch + Duration::from_millis(d));

        match step.action {
            Some(PttAction::Start) => {
                if let Some(rec) = current.take() {
                    discard(&app, rec);
                }
                match start(&app, at).await {
                    Ok(rec) => current = Some(rec),
                    Err(message) => {
                        ptt.reset();
                        deadline = None;
                        overlay::claim(&app, OverlayState::Error { message });
                    }
                }
            }
            Some(PttAction::Stop) => {
                if let Some(rec) = current.take() {
                    stop(&app, rec, at);
                }
            }
            Some(PttAction::Discard) => {
                if let Some(rec) = current.take() {
                    discard(&app, rec);
                }
            }
            None => {}
        }
        hotkey::set_recording(current.is_some());
    }
}

async fn start(app: &AppHandle, pressed: Instant) -> Result<Recording, String> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let provider = settings.stt.provider;
    let (stt_secrets, stt_keys) = stt_keys(&state, provider);
    if stt_keys.is_empty() {
        let provider = provider.label();
        return Err(tr!("No {provider} key — add it in the settings", "Brak klucza {provider} — dodaj go w ustawieniach"));
    }
    // Held from the key press to the end of paste; blocks the update swap.
    let busy = state
        .try_begin_dictation()
        .ok_or_else(|| tr!("An update is being installed — try again in a moment", "Trwa instalacja aktualizacji — spróbuj za chwilę"))?;
    let target = paste::foreground();

    let (tx, rx) = mpsc::unbounded_channel();
    state.recorder.start(tx).await.map_err(|e| tr!("Microphone: {e}", "Mikrofon: {e}"))?;
    hotkey::set_recording(true);

    let overlay_id = overlay::claim(
        app,
        OverlayState::Recording {
            mode: settings.active_mode().name.clone(),
            lang: settings.ui_language,
            switchable: settings.next_mode().is_some(),
        },
    );
    let start_cue = settings.audio.cues.then(|| {
        tauri::async_runtime::spawn(async {
            tokio::time::sleep(START_CUE_DELAY).await;
            sound::play(Cue::Start);
        })
    });

    let meter = {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tick.tick().await;
                let level = app.state::<AppState>().recorder.level();
                overlay::level(&app, level);
            }
        })
    };

    let released = Arc::new(Mutex::new(None));
    let pipeline = {
        let app = app.clone();
        let released = released.clone();
        tauri::async_runtime::spawn(async move {
            let _busy = busy;
            let state = app.state::<AppState>();
            let min_samples = settings.audio.min_duration_ms as usize * TARGET_RATE as usize / 1000;
            let stt = transcribe(&state.http, &settings.stt, &stt_keys, rx, min_samples).await;
            // The indicator can switch the mode while recording, so the mode
            // is read once the recording is over.
            let mode = state.settings().active_mode().clone();

            let mut tl = Timeline::starting_at(pressed);
            tl.released = Some(released.lock().expect("released lock").unwrap_or_else(Instant::now));
            let mut meta = TimingMeta {
                mode: mode.id.clone(),
                stt: provider.label().into(),
                ..TimingMeta::default()
            };

            let outcome = match stt {
                Ok(o) => o,
                Err(Error::NoSpeech) => {
                    tracing::info!("nagranie bez mowy, pomijam");
                    overlay::hide_for(&app, overlay_id);
                    return;
                }
                Err(e) => {
                    tracing::warn!("STT {}: {e}", provider.label());
                    meta.outcome = "stt_error".into();
                    let report = tl.report(meta);
                    crate::logging::record_timing(&report);
                    state.push_timing(report);
                    overlay::set_for(&app, overlay_id, OverlayState::Error { message: tr!("Transcription: {e}", "Transkrypcja: {e}") });
                    return;
                }
            };
            tl.stt_done = Some(Instant::now());
            tl.stt_connected = outcome.connected_at;
            meta.streamed = outcome.streamed;
            if let Some((usd, estimated)) = stt_cost(&settings.stt, &outcome) {
                meta.stt_cost_usd = Some(usd);
                meta.stt_cost_estimated = estimated;
            }
            if let Some(r) = &outcome.fallback_reason {
                tracing::warn!("{}: fallback do trybu wsadowego: {r}", provider.label());
            }
            // The key tried first failed: say which one took over, and keep it first.
            let switched_to = (outcome.key_index > 0).then(|| {
                let used = stt_secrets[outcome.key_index];
                *PREFERRED_STT_KEY.lock().expect("preferred STT key") = Some(used);
                let which = if used == SecretKey::ElevenLabsBackup {
                    tr!("backup key", "klucz zapasowy")
                } else {
                    tr!("main key", "klucz główny")
                };
                tracing::warn!("{}: przełączono na {which}", provider.label());
                which
            });
            let stt_model = if outcome.streamed { settings.stt.realtime_model() } else { settings.stt.batch_model() };
            let stt_model = stt_model.to_string();
            let transcript = outcome.text.clone();

            let mut text = outcome.text;
            let mut warning = None;
            meta.outcome = "ok".into();
            if mode.post_process {
                overlay::set_for(&app, overlay_id, OverlayState::Processing { stage: tr!("Cleaning up", "Porządkowanie") });
                let llm = settings.llm.for_mode(&mode);
                meta.llm = llm_label(&llm);
                let llm_key = llm.secret().and_then(|k| state.secrets.get(k));
                match post_process(&state.http, &llm, &mode, &settings.stt.dictionary, &text, llm_key.as_deref()).await {
                    Ok(out) if !out.text.trim().is_empty() => {
                        tl.llm_first_token = out.first_token;
                        meta.llm_cost_usd = out.cost_usd;
                        text = out.text;
                    }
                    Ok(out) => {
                        meta.llm_cost_usd = out.cost_usd;
                        meta.outcome = "llm_empty_raw".into();
                        warning = Some(tr!(
                            "Post-processing returned an empty result — pasted the raw transcript",
                            "Post-processing zwrócił pusty wynik — wklejono surową transkrypcję"
                        ));
                    }
                    Err(e) => {
                        tracing::warn!("post-processing {}: {e}", meta.llm);
                        meta.outcome = "llm_error_raw".into();
                        warning = Some(tr!("Post-processing: {e} — pasted the raw transcript", "Post-processing: {e} — wklejono surową transkrypcję"));
                    }
                }
                tl.llm_done = Some(Instant::now());
            }
            // Also covers modes without AI and a raw transcript after a failed post-processing.
            text = hyperrust_core::dictionary::apply(&text, &settings.stt.dictionary);
            meta.chars = text.chars().count();
            let output = text.clone();

            // The lock covers the clipboard restore too: the next dictation
            // must not snapshot our temporary text as the user's clipboard.
            let _paste = PASTE_LOCK.lock().await;
            let paste_settings = settings.paste.clone();
            let auto_paste = paste_settings.auto_paste;
            let delivered = tokio::task::spawn_blocking(move || paste::deliver(&text, &paste_settings, target))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            tl.pasted = Some(Instant::now());
            let pending_restore = match delivered {
                Ok(restore) => restore,
                Err(e) => {
                    meta.outcome = "paste_error".into();
                    warning = Some(tr!("Paste: {e}", "Wklejanie: {e}"));
                    None
                }
            };
            let report = tl.report(meta);
            crate::logging::record_timing(&report);
            state.push_timing(report.clone());
            let done = if auto_paste { tr!("Pasted", "Wklejono") } else { tr!("Copied to the clipboard", "Skopiowano do schowka") };
            let done = match switched_to {
                Some(key) => format!("{done} · {key}"),
                None => done,
            };
            match warning {
                Some(message) => overlay::set_for(&app, overlay_id, OverlayState::Error { message }),
                None => overlay::set_for(&app, overlay_id, OverlayState::Done { message: done }),
            }
            let ts_ms = history::now_ms();
            let device = history::device();
            history::record(
                app.clone(),
                settings.history.clone(),
                HistoryEntry {
                    id: HistoryEntry::id_for(&device, ts_ms),
                    ts_ms,
                    device,
                    build: BUILD.build.into(),
                    stt_model,
                    prompt_sha: if mode.post_process { prompt_sha(mode.system_prompt()) } else { String::new() },
                    transcript,
                    output,
                    timing: report,
                },
            );
            if let Some(restore) = pending_restore {
                let _ = tokio::task::spawn_blocking(move || restore.finish()).await;
            }
        })
    };

    Ok(Recording { released, pipeline, meter, start_cue, overlay_id })
}

/// The provider's stored keys and their values, the one that last took over
/// first, then in the provider's order.
fn stt_keys(state: &AppState, provider: SttProviderKind) -> (Vec<SecretKey>, Vec<String>) {
    let mut order = provider.secrets();
    let preferred = *PREFERRED_STT_KEY.lock().expect("preferred STT key");
    if let Some(i) = order.iter().position(|&k| Some(k) == preferred) {
        order.rotate_left(i);
    }
    order.into_iter().filter_map(|k| state.secrets.get(k).map(|v| (k, v))).unzip()
}

fn stop(app: &AppHandle, rec: Recording, released: Instant) {
    *rec.released.lock().expect("released lock") = Some(released);
    app.state::<AppState>().recorder.stop();
    rec.meter.abort();
    // Pair the stop cue with a start cue the user actually heard.
    if let Some(cue) = rec.start_cue {
        if cue.inner().is_finished() {
            sound::play(Cue::Stop);
        } else {
            cue.abort();
        }
    }
    overlay::set_for(app, rec.overlay_id, OverlayState::Processing { stage: tr!("Transcribing", "Transkrypcja") });
    hotkey::set_recording(false);
}

fn discard(app: &AppHandle, rec: Recording) {
    // Abort first: closing the audio channel alone would look like a release.
    rec.pipeline.abort();
    rec.meter.abort();
    if let Some(cue) = rec.start_cue {
        cue.abort();
    }
    app.state::<AppState>().recorder.stop();
    overlay::hide_for(app, rec.overlay_id);
    hotkey::set_recording(false);
}

/// Keep TLS connections to the configured providers warm while idle.
pub async fn keep_warm(app: AppHandle) {
    let mut tick = tokio::time::interval(Duration::from_secs(45));
    loop {
        tick.tick().await;
        let state = app.state::<AppState>();
        let s = state.settings();
        if !s.keep_connections_warm || !state.is_idle(5_000) {
            continue;
        }
        let mut urls = vec![s.stt.provider.base_url().to_string()];
        if let Some((base, _)) = s.llm.http_target() {
            urls.push(base.to_string());
        }
        for url in urls {
            if let Err(e) = hyperrust_core::http::warm_up(&state.http, &url).await {
                tracing::debug!("rozgrzewanie {url}: {e}");
            }
        }
    }
}

/// Hotkey mode for the tray label.
pub fn mode_label(mode: HotkeyMode) -> String {
    match mode {
        HotkeyMode::Auto => tr!("hold or double-tap", "przytrzymaj lub stuknij dwa razy"),
        HotkeyMode::Hold => tr!("hold", "przytrzymaj"),
        HotkeyMode::Toggle => tr!("press to start and stop", "naciśnij, aby zacząć i skończyć"),
    }
}

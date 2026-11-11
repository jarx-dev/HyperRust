//! Commands for the settings window.

use std::collections::HashMap;

use hyperrust_core::balance::{self, ElevenLabsBalance, OpenRouterBalance, UsageRow};
use hyperrust_core::catalog::{self, CatalogModel, NewModel, SttModels};
use hyperrust_core::config::{LlmBackend, SecretKey, OPENROUTER_BASE_URL};
use hyperrust_core::history::prompt_sha;
use hyperrust_core::hotkey::HotkeySpec;
use hyperrust_core::modes::{Preset, AGENT_PROMPT};
use hyperrust_core::ranking::{self, Ranking};
use hyperrust_core::stt::SttProviderKind;
use hyperrust_core::timing::TimingReport;
use hyperrust_core::update::{self, BuildInfo};
use hyperrust_core::Settings;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::balances::{self, Fetched};
use crate::controller::CtlMsg;
use crate::state::{AppState, BUILD};
use crate::updater::UpdateStatus;
use crate::{autostart, hotkey, paths, store, tray};

#[derive(Serialize)]
pub struct Snapshot {
    settings: Settings,
    /// Built-in prompt per preset (`agent`, `cleanup`), shown when a mode's
    /// own prompt is empty.
    builtin_prompts: HashMap<String, String>,
    build: BuildInfo,
    /// [`crate::state::version_label`], e.g. `v26.1111.1215`.
    version: String,
    /// Which keys are stored (never their values).
    secrets: HashMap<SecretKey, bool>,
    logs_dir: String,
    update: UpdateStatus,
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    let mut settings = state.settings();
    // Show what Windows will actually do: autostart may be off in Task Manager.
    if let Some(active) = autostart::is_active() {
        settings.autostart = active;
    }
    let builtin_prompts = [("agent", Preset::Agent), ("cleanup", Preset::Cleanup)]
        .into_iter()
        .map(|(name, preset)| (name.to_string(), preset.prompt().to_string()))
        .collect();
    Snapshot {
        settings,
        builtin_prompts,
        build: BUILD,
        version: crate::state::version_label(),
        secrets: SecretKey::ALL.iter().map(|&k| (k, state.secrets.is_set(k))).collect(),
        logs_dir: paths::logs_dir().display().to_string(),
        update: state.update.status(),
    }
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> Result<Settings, String> {
    HotkeySpec::parse(&settings.hotkey).map_err(|e| format!("skrót „{}”: {e}", settings.hotkey))?;
    if settings.updates.enabled {
        hyperrust_core::update::validate_url(&settings.updates.url).map_err(|e| e.to_string())?;
    }
    if let Some(url) = settings.history.upload_target() {
        hyperrust_core::update::validate_private_url(url, "historii").map_err(|e| e.to_string())?;
    }
    let settings = settings.normalized();
    store::save(&settings)?;
    let previous = std::mem::replace(&mut *state.settings.write().expect("settings lock"), settings.clone());

    // Ticking the box also re-enables an entry switched off in Task Manager.
    if previous.autostart != settings.autostart || (settings.autostart && autostart::is_active() == Some(false)) {
        autostart::set_enabled(settings.autostart, true)?;
    }
    if previous.audio != settings.audio {
        state.recorder.configure(settings.audio.clone());
    }
    if previous.hotkey != settings.hotkey || previous.hotkey_mode != settings.hotkey_mode {
        let _ = state.ctl.send(CtlMsg::Reconfigure);
    }
    if previous.updates != settings.updates {
        state.update.request_check();
    }
    if balances::needs_fetch(&previous, &settings) {
        balances::refresh(&app, true);
    }
    tray::refresh(&app);
    Ok(settings)
}

#[tauri::command]
pub fn set_secret(app: AppHandle, state: State<'_, AppState>, key: SecretKey, value: String) -> Result<(), String> {
    state.secrets.set(key, &value)?;
    key_changed(&app, key);
    Ok(())
}

#[tauri::command]
pub fn delete_secret(app: AppHandle, state: State<'_, AppState>, key: SecretKey) -> Result<(), String> {
    state.secrets.delete(key)?;
    key_changed(&app, key);
    Ok(())
}

/// A changed key may work again, so dictation starts from the main key; the
/// tray tooltip shows the balance behind some keys.
fn key_changed(app: &AppHandle, key: SecretKey) {
    crate::controller::reset_preferred_stt_key();
    if matches!(key, SecretKey::ElevenLabs | SecretKey::ElevenLabsBackup | SecretKey::OpenRouter) {
        balances::refresh(app, true);
    }
}

/// Models of the selected post-processing endpoint (`GET /models`), with
/// prices where the endpoint lists them (OpenRouter).
#[tauri::command]
pub async fn list_llm_models(
    state: State<'_, AppState>,
    backend: LlmBackend,
    base_url: String,
) -> Result<Vec<CatalogModel>, String> {
    let (base, key) = match backend {
        LlmBackend::OpenRouter => (OPENROUTER_BASE_URL.to_string(), state.secrets.get(SecretKey::OpenRouter)),
        LlmBackend::OpenAiCompatible => (base_url, state.secrets.get(SecretKey::LlmEndpoint)),
        LlmBackend::ClaudeCli | LlmBackend::CodexCli => return Ok(Vec::new()),
    };
    catalog::get_list(catalog::models_request(&state.http, &base, key.as_deref()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_input_devices(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.recorder.devices().await)
}

/// STT models of `provider` for the pickers: from its API where there is one.
#[tauri::command]
pub async fn list_stt_models(state: State<'_, AppState>, provider: SttProviderKind) -> Result<SttModels, String> {
    let key = state.secrets.get(provider.secret());
    catalog::stt_models(&state.http, provider, key.as_deref()).await.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct ModelNews {
    days: u64,
    stt: Vec<NewModel>,
    stt_error: Option<String>,
    /// Where the post-processing models come from; empty for the CLI backends.
    llm_source: &'static str,
    llm: Vec<NewModel>,
    llm_error: Option<String>,
}

const NEWS_LIMIT: usize = 8;

/// Models added recently: speech-to-text from OpenRouter's public catalog
/// (it lists every vendor), post-processing from the active endpoint (only
/// chat models: OpenRouter also lists image models, routers and variants).
/// Asked for by the overview page only.
#[tauri::command]
pub async fn get_model_news(state: State<'_, AppState>) -> Result<ModelNews, String> {
    let s = state.settings();
    let now = crate::history::now_ms() / 1000;
    let http = &state.http;
    let live = s.stt.streaming && s.stt.provider != SttProviderKind::OpenRouter;
    let stt_model = if live { s.stt.realtime_model() } else { s.stt.batch_model() };
    let stt_yours = catalog::openrouter_stt_id(s.stt.provider, stt_model);
    let (llm_source, llm_request, llm_yours) = match s.llm.backend {
        LlmBackend::OpenAiCompatible => {
            let key = state.secrets.get(SecretKey::LlmEndpoint);
            let rb = catalog::models_request(http, &s.llm.base_url, key.as_deref());
            ("CLIProxy", Some(rb), s.llm.model.clone())
        }
        LlmBackend::OpenRouter => (
            "OpenRouter",
            Some(http.get(catalog::OPENROUTER_MODELS_URL)),
            catalog::openrouter_base_id(&s.llm.openrouter_model).to_string(),
        ),
        LlmBackend::ClaudeCli | LlmBackend::CodexCli => ("", None, String::new()),
    };
    let (stt, llm) = tokio::join!(catalog::get_list(http.get(catalog::OPENROUTER_STT_URL)), async {
        match llm_request {
            Some(rb) => catalog::get_list(rb).await.map(Some),
            None => Ok(None),
        }
    });
    let (stt, stt_error) = match stt {
        Ok(models) => (catalog::recent(&models, &stt_yours, now, NEWS_LIMIT), None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let (llm, llm_error) = match llm {
        Ok(models) => {
            let chat: Vec<_> = models.unwrap_or_default().into_iter().filter(|m| m.chat).collect();
            (catalog::recent(&chat, &llm_yours, now, NEWS_LIMIT), None)
        }
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    Ok(ModelNews { days: catalog::NEW_DAYS, stt, stt_error, llm_source, llm, llm_error })
}

#[tauri::command]
pub fn recent_timings(state: State<'_, AppState>) -> Vec<TimingReport> {
    state.recent_timings()
}

#[derive(Serialize)]
pub struct RankingView {
    #[serde(flatten)]
    ranking: Ranking,
    /// This build's agent prompt; the scores belong to `prompt_sha`.
    app_prompt_sha: String,
    /// Whole days since the ranking job wrote the ranking.
    age_days: u64,
}

/// The post-processing model ranking: next to a private update manifest, or
/// its public copy on GitHub (see `hyperrust_core::ranking`).
#[tauri::command]
pub async fn get_model_ranking(state: State<'_, AppState>) -> Result<RankingView, String> {
    let url = ranking::url(&state.settings().updates.url).map_err(|e| e.to_string())?;
    let client = update::client().map_err(|e| e.to_string())?;
    let ranking = ranking::fetch(&client, &url).await.map_err(|e| e.to_string())?;
    let age_days = (crate::history::now_ms() / 1000).saturating_sub(ranking.generated_at) / 86_400;
    Ok(RankingView { ranking, app_prompt_sha: prompt_sha(AGENT_PROMPT), age_days })
}

/// The dictation history kept on this computer, newest first.
#[tauri::command]
pub async fn get_history() -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(crate::history::local)
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Overview {
    elevenlabs: Fetched<ElevenLabsBalance>,
    /// The second ElevenLabs account dictation moves to when the first fails.
    elevenlabs_backup: Fetched<ElevenLabsBalance>,
    openrouter: Fetched<OpenRouterBalance>,
    /// Dictations of the last 30 days on this computer, oldest first.
    usage: Vec<UsageRow>,
    /// Unix seconds.
    fetched_at: u64,
}

const USAGE_DAYS: u64 = 30;

/// Balances of the paid providers and this computer's recent usage, for the
/// overview page. Also refreshes the tray tooltip.
#[tauri::command]
pub async fn get_overview(app: AppHandle) -> Result<Overview, String> {
    let now = crate::history::now_ms() / 1000;
    let since = now.saturating_sub(USAGE_DAYS * 86_400);
    let ((elevenlabs, elevenlabs_backup, openrouter), usage) = tokio::join!(
        balances::fetch(&app, now),
        tokio::task::spawn_blocking(move || match std::fs::read_to_string(paths::timings_file()) {
            Ok(log) => balance::usage_rows(&log, since),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("odczyt timings.jsonl: {e}");
                }
                Vec::new()
            }
        }),
    );
    Ok(Overview {
        elevenlabs,
        elevenlabs_backup,
        openrouter,
        usage: usage.map_err(|e| e.to_string())?,
        fetched_at: now,
    })
}

/// Pause the global shortcut while the settings window records a new one.
#[tauri::command]
pub fn set_hotkey_capture(active: bool) {
    hotkey::set_paused(active);
}

/// Click on the recording indicator: same as the shortcut's stop press.
#[tauri::command]
pub fn finish_dictation(state: State<'_, AppState>) {
    let _ = state.ctl.send(CtlMsg::Finish);
}

/// Click on the mode name in the recording indicator: switch to the next mode
/// for good, including the dictation in progress. Returns the new mode's name.
#[tauri::command]
pub fn cycle_mode(app: AppHandle, state: State<'_, AppState>) -> Option<String> {
    let next = state.settings().next_mode()?.clone();
    tray::set_active_mode(&app, &next.id);
    Some(next.name)
}

#[tauri::command]
pub fn check_updates(state: State<'_, AppState>) -> UpdateStatus {
    state.update.request_check();
    state.update.status()
}

#[tauri::command]
pub fn open_logs_dir() -> Result<(), String> {
    let dir = paths::logs_dir();
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens one of the app's own pages in the default browser. The page names a
/// link, never a URL, so the webview cannot open arbitrary addresses.
#[tauri::command]
pub fn open_link(link: &str) -> Result<(), String> {
    let url = match link {
        "repo" => "https://github.com/jarx-dev/HyperRusts",
        "coffee" => "https://buymeacoffee.com/jarx",
        _ => return Err(format!("nieznany link „{link}”")),
    };
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

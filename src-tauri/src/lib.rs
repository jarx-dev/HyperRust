//! HyperRust: dictate prompts for AI agents. Hold the shortcut, speak, release;
//! the (optionally post-processed) transcript is pasted into the active window.
//!
//! The whole dictation path is Rust: keyboard hook → microphone → streaming
//! STT → post-processing → clipboard paste. The webviews only render the
//! settings window (created on demand) and the recording indicator.

mod autostart;
mod balances;
mod commands;
mod controller;
mod history;
mod hotkey;
mod logging;
mod overlay;
mod paste;
mod paths;
mod recorder;
mod secrets;
mod sound;
mod state;
mod store;
mod tray;
mod updater;

use hyperrust_core::hotkey::HotkeySpec;
use hyperrust_core::Settings;
use tauri::RunEvent;
use tokio::sync::mpsc;

use crate::recorder::Recorder;
use crate::state::{AppState, BUILD};
use crate::updater::UpdateHandle;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    logging::init();
    if updater::run_guard(&args) {
        return;
    }
    let after_update = updater::finish_pending_update(&args);
    tracing::info!("HyperRust {} (build {}){}", state::version_label(), BUILD.build, if after_update { ", po aktualizacji" } else { "" });

    let first_run = !paths::settings_file().exists();
    let settings = store::load();
    let http = hyperrust_core::http::build_client().expect("klient HTTP");
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
    let recorder = Recorder::spawn(settings.audio.clone(), ctl_tx.clone());
    let state = AppState::new(settings.clone(), http, recorder, ctl_tx.clone(), UpdateHandle::default());
    state.secrets.preload();
    let missing_stt_key = !state.secrets.is_set(settings.stt.provider.secret());

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| tray::open_settings(app)))
        .manage(state)
        .setup(move |app| {
            let handle = app.handle().clone();
            overlay::init(&handle);
            tray::build(&handle)?;
            if let Err(e) = autostart::set_enabled(settings.autostart, false) {
                tracing::warn!("autostart: {e}");
            }
            let spec = HotkeySpec::parse(&settings.hotkey)
                .or_else(|_| HotkeySpec::parse(&Settings::default().hotkey))
                .expect("domyślny skrót");
            if let Err(e) = hotkey::start(spec, ctl_tx) {
                tracing::error!("globalny skrót: {e}");
            }
            tauri::async_runtime::spawn(controller::run(handle.clone(), ctl_rx));
            tauri::async_runtime::spawn(controller::keep_warm(handle.clone()));
            tauri::async_runtime::spawn(updater::run(handle.clone()));
            {
                // Balances for the tray tooltip, once the start-up rush is over.
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    balances::refresh(&handle, true);
                });
            }
            if first_run || missing_stt_key {
                tray::open_settings(&handle);
            }
            updater::startup_complete(after_update);
            Ok(())
        })
        .on_window_event(|window, event| {
            // A settings window closed mid-capture must not leave the shortcut paused.
            if window.label() == "settings" && matches!(event, tauri::WindowEvent::Destroyed) {
                hotkey::set_paused(false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::save_settings,
            commands::set_secret,
            commands::delete_secret,
            commands::list_llm_models,
            commands::list_input_devices,
            commands::list_stt_models,
            commands::get_model_news,
            commands::get_model_ranking,
            commands::recent_timings,
            commands::get_history,
            commands::get_overview,
            commands::set_hotkey_capture,
            commands::finish_dictation,
            commands::cycle_mode,
            commands::check_updates,
            commands::open_logs_dir,
            commands::open_link,
        ])
        .build(tauri::generate_context!())
        .expect("uruchomienie aplikacji Tauri");

    app.run(|_app, event| {
        // Closing the settings window must not end the tray app.
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
        }
    });
}

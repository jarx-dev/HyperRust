//! Tray icon: settings, history, mode switch, update check, logs, quit; the
//! tooltip shows the active mode and the provider balances.

use hyperrust_core::balance;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::controller::mode_label;
use crate::state::AppState;
use crate::store;

const TRAY_ID: &str = "main-tray";
const SETTINGS_LABEL: &str = "settings";
const MODE_PREFIX: &str = "mode:";

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let settings = app.state::<AppState>().settings();
    let modes = Submenu::with_id(app, "modes", "Tryb", true)?;
    for m in &settings.modes {
        let item = CheckMenuItem::with_id(
            app,
            format!("{MODE_PREFIX}{}", m.id),
            &m.name,
            true,
            m.id == settings.active_mode,
            None::<&str>,
        )?;
        modes.append(&item)?;
    }
    let open = MenuItem::with_id(app, "settings", "Ustawienia…", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "Historia…", true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Uruchamiaj z Windows",
        true,
        crate::autostart::is_active().unwrap_or(settings.autostart),
        None::<&str>,
    )?;
    let update = MenuItem::with_id(app, "update", "Sprawdź aktualizacje", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Otwórz folder logów", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Zakończ", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(app, &[&open, &history, &modes, &autostart, &sep, &update, &logs, &sep2, &quit])
}

/// Mode and provider balances first; shortcut and version only while they
/// fit the Windows tooltip limit.
fn tooltip(app: &AppHandle) -> String {
    let s = app.state::<AppState>().settings();
    let mut required = vec![format!("HyperRust — tryb: {}", s.active_mode().name)];
    required.extend(crate::balances::lines(&s));
    let optional = [format!("Skrót: {} ({})", s.hotkey, mode_label(s.hotkey_mode)), format!("Wersja: {}", crate::state::version_label())];
    balance::fit_tooltip(&required, &optional, balance::TOOLTIP_MAX)
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().expect("default window icon").clone())
        .tooltip(tooltip(app))
        .menu(&build_menu(app)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            match id {
                "settings" => open_settings(app),
                "history" => open_page(app, "history"),
                "autostart" => toggle_autostart(app),
                "update" => app.state::<AppState>().update.request_check(),
                "logs" => {
                    let _ = crate::commands::open_logs_dir();
                }
                "quit" => app.exit(0),
                _ => {
                    if let Some(mode) = id.strip_prefix(MODE_PREFIX) {
                        set_active_mode(app, mode);
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                open_settings(tray.app_handle());
            }
            // The tooltip appears a moment later; a stale balance is fetched meanwhile.
            TrayIconEvent::Enter { .. } => crate::balances::refresh(tray.app_handle(), false),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Rebuild the menu and tooltip after a settings change.
pub fn refresh(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
        let _ = tray.set_tooltip(Some(tooltip(app)));
    }
}

/// New balances: only the tooltip changes.
pub fn refresh_tooltip(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip(app)));
    }
}

/// Switch the active mode, save it and tell the tray and an open settings window.
pub fn set_active_mode(app: &AppHandle, mode: &str) {
    let state = app.state::<AppState>();
    let updated = {
        let mut s = state.settings.write().expect("settings lock");
        if s.modes.iter().any(|m| m.id == mode) {
            s.active_mode = mode.to_string();
        }
        s.clone()
    };
    if let Err(e) = store::save(&updated) {
        tracing::warn!("zapis ustawień: {e}");
    }
    refresh(app);
    // Only the mode changed: an open settings window updates that one field
    // and keeps the user's unsaved edits.
    let _ = tauri::Emitter::emit(app, "active-mode-changed", &updated.active_mode);
}

/// Flip launch at login from the tray menu and tell an open settings window.
fn toggle_autostart(app: &AppHandle) {
    let state = app.state::<AppState>();
    let enabled = !crate::autostart::is_active().unwrap_or_else(|| state.settings().autostart);
    if let Err(e) = crate::autostart::set_enabled(enabled, true) {
        tracing::warn!("autostart: {e}");
        refresh(app); // put the check mark back
        return;
    }
    let updated = {
        let mut s = state.settings.write().expect("settings lock");
        s.autostart = enabled;
        s.clone()
    };
    if let Err(e) = store::save(&updated) {
        tracing::warn!("zapis ustawień: {e}");
    }
    refresh(app);
    let _ = tauri::Emitter::emit(app, "autostart-changed", enabled);
}

/// The settings window is created on demand and destroyed on close, so it
/// costs nothing while the user just dictates.
pub fn open_settings(app: &AppHandle) {
    show_window(app, None);
}

/// Open the settings window on `page` (a `data-page` of `index.html`).
pub fn open_page(app: &AppHandle, page: &str) {
    show_window(app, Some(page));
}

fn show_window(app: &AppHandle, page: Option<&str>) {
    if let Some(w) = app.get_webview_window(SETTINGS_LABEL) {
        if let Some(page) = page {
            let _ = tauri::Emitter::emit_to(app, SETTINGS_LABEL, "navigate", page);
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let url = page.map_or_else(|| "index.html".to_string(), |p| format!("index.html#{p}"));
    let result = WebviewWindowBuilder::new(app, SETTINGS_LABEL, WebviewUrl::App(url.into()))
        .title("HyperRust — ustawienia")
        .inner_size(1000.0, 760.0)
        .min_inner_size(760.0, 540.0)
        .center()
        .build();
    match result {
        // Windows' foreground lock can open a new window behind the active app
        // (only its taskbar button flashes); `set_focus` forces it to the front.
        Ok(w) => {
            let _ = w.set_focus();
        }
        Err(e) => tracing::warn!("okno ustawień: {e}"),
    }
}

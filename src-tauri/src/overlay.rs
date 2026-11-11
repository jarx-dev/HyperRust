//! Recording indicator: a tiny always-on-top webview created hidden at start-up
//! and shown without ever taking focus, so the dictation target keeps the
//! caret and Ctrl+V lands in the right place.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

pub const LABEL: &str = "overlay";
/// Bumped on every state change so a delayed hide never hides a newer state.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Dictation that owns the indicator. A dictation still processing after a
/// newer recording started must not overwrite or hide the newer one.
static OWNER: Mutex<u64> = Mutex::new(0);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OverlayState {
    /// Sent just before hiding, so the next show cannot paint a stale state.
    Hidden,
    Recording {
        mode: String,
        /// More than one mode: clicking the name switches to the next one.
        switchable: bool,
    },
    Processing { stage: String },
    Done { message: String },
    Error { message: String },
}

/// One-time window setup (click-through, no activation).
pub fn init(app: &AppHandle) {
    let Some(w) = app.get_webview_window(LABEL) else { return };
    let _ = w.set_ignore_cursor_events(true);
    platform::make_non_activating(&w);
}

/// Hand the indicator to a new dictation and show its first state.
pub fn claim(app: &AppHandle, state: OverlayState) -> u64 {
    let mut owner = OWNER.lock().expect("overlay owner");
    *owner += 1;
    set(app, state);
    *owner
}

/// Update the indicator on behalf of dictation `id`, if it still owns it.
pub fn set_for(app: &AppHandle, id: u64, state: OverlayState) {
    let owner = OWNER.lock().expect("overlay owner");
    if *owner == id {
        set(app, state);
    }
}

pub fn hide_for(app: &AppHandle, id: u64) {
    let owner = OWNER.lock().expect("overlay owner");
    if *owner == id {
        hide(app);
    }
}

pub fn set(app: &AppHandle, state: OverlayState) {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let _ = app.emit_to(LABEL, "overlay-state", &state);
    if let Some(w) = app.get_webview_window(LABEL) {
        // Only a recording can be finished by clicking the indicator.
        platform::set_click_through(&w, !matches!(state, OverlayState::Recording { .. }));
        platform::show_no_activate(&w);
    }
    // src/overlay.ts fades the pill out just before these delays end.
    let linger = match state {
        OverlayState::Done { .. } => Some(Duration::from_millis(600)),
        OverlayState::Error { .. } => Some(Duration::from_millis(3500)),
        _ => None,
    };
    if let Some(d) = linger {
        hide_after(app, d);
    }
}

pub fn level(app: &AppHandle, level: f32) {
    let _ = app.emit_to(LABEL, "overlay-level", level);
}

pub fn hide(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let _ = app.emit_to(LABEL, "overlay-state", &OverlayState::Hidden);
    if let Some(w) = app.get_webview_window(LABEL) {
        platform::hide(&w);
    }
}

fn hide_after(app: &AppHandle, delay: Duration) {
    let generation = GENERATION.load(Ordering::Relaxed);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        if GENERATION.load(Ordering::Relaxed) == generation {
            hide(&app);
        }
    });
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    // The window is shown, hidden and made click-through with Win32 calls
    // only. tao never learns the window became visible, so its `hide()` is a
    // no-op, and its `set_ignore_cursor_events` rebuilds the whole extended
    // style, dropping WS_EX_NOACTIVATE and WS_EX_TOOLWINDOW set below.

    fn hwnd(w: &tauri::WebviewWindow) -> Option<HWND> {
        w.hwnd().ok().map(|h| h.0 as HWND)
    }

    fn update_ex_style(h: HWND, f: impl FnOnce(isize) -> isize) {
        // SAFETY: valid HWND of our own window; only style bits are changed.
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let new = f(ex);
            if new != ex {
                SetWindowLongPtrW(h, GWL_EXSTYLE, new);
            }
        }
    }

    pub fn make_non_activating(w: &tauri::WebviewWindow) {
        let Some(h) = hwnd(w) else { return };
        update_ex_style(h, |ex| ex | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize);
    }

    /// Mouse clicks pass through to the window below (`true`) or reach the
    /// indicator (`false`); WS_EX_NOACTIVATE keeps the focus where it was.
    /// Same two bits as tao's `set_ignore_cursor_events`, the rest is kept.
    pub fn set_click_through(w: &tauri::WebviewWindow, through: bool) {
        let Some(h) = hwnd(w) else { return };
        let bits = (WS_EX_TRANSPARENT | WS_EX_LAYERED) as isize;
        update_ex_style(h, |ex| if through { ex | bits } else { ex & !bits });
    }

    pub fn hide(w: &tauri::WebviewWindow) {
        set_click_through(w, true);
        let Some(h) = hwnd(w) else { return };
        // SAFETY: valid HWND of our own window.
        unsafe { ShowWindow(h, SW_HIDE) };
    }

    /// Bottom-centre of the monitor the user is working on, topmost, without activation.
    pub fn show_no_activate(w: &tauri::WebviewWindow) {
        let Some(h) = hwnd(w) else { return };
        // SAFETY: Win32 geometry calls on valid handles with properly sized out-structs.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(h, &mut rect);
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
            let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            let (x, y) = if GetMonitorInfoW(monitor, &mut info) != 0 {
                let work = info.rcWork;
                // The window has a transparent margin around the pill.
                ((work.left + work.right - width) / 2, work.bottom - height - height / 2)
            } else {
                (rect.left, rect.top)
            };
            SetWindowPos(h, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn make_non_activating(_w: &tauri::WebviewWindow) {}

    pub fn set_click_through(w: &tauri::WebviewWindow, through: bool) {
        let _ = w.set_ignore_cursor_events(through);
    }

    pub fn show_no_activate(w: &tauri::WebviewWindow) {
        let _ = w.show();
    }

    pub fn hide(w: &tauri::WebviewWindow) {
        let _ = w.hide();
    }
}

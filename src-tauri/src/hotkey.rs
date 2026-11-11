//! The global push-to-talk key: a low-level keyboard hook (`WH_KEYBOARD_LL`) on
//! a dedicated thread. Unlike `RegisterHotKey` it sees key
//! releases (needed for hold-to-talk), bare modifiers and Escape, and can
//! swallow the shortcut so the focused app never receives it.
//!
//! The hook callback only runs the matcher from `hyperrust-core` and forwards
//! events; all timing logic lives in the controller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use hyperrust_core::hotkey::{HotkeySpec, KeyEvent, KeyMatcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::controller::CtlMsg;

pub type HookEvent = (KeyEvent, Instant);

static MATCHER: Mutex<Option<KeyMatcher>> = Mutex::new(None);
static RECORDING: AtomicBool = AtomicBool::new(false);
/// Set while the settings window captures a new shortcut.
static PAUSED: AtomicBool = AtomicBool::new(false);
static EVENTS: OnceLock<UnboundedSender<CtlMsg>> = OnceLock::new();

pub fn set_spec(spec: HotkeySpec) {
    let mut guard = MATCHER.lock().expect("hotkey mutex");
    match guard.as_mut() {
        Some(m) => m.set_spec(spec),
        None => *guard = Some(KeyMatcher::new(spec)),
    }
}

/// Enables Escape-to-cancel while a recording runs.
pub fn set_recording(on: bool) {
    RECORDING.store(on, Ordering::Relaxed);
}

pub fn set_paused(on: bool) {
    if on {
        // The release of a shortcut held right now will never be seen: drop
        // any recording and reset push-to-talk instead of leaving it running.
        if !PAUSED.swap(true, Ordering::Relaxed) {
            if let Some(tx) = EVENTS.get() {
                let _ = tx.send(CtlMsg::Reconfigure);
            }
        }
        return;
    }
    if !PAUSED.load(Ordering::Relaxed) {
        return;
    }
    // Reset before the hook resumes: key-ups during the capture were lost.
    if let Some(m) = MATCHER.lock().expect("hotkey mutex").as_mut() {
        m.reset();
    }
    PAUSED.store(false, Ordering::Relaxed);
}

/// Install the hook. Events go straight to the controller, stamped with the
/// time they were observed.
pub fn start(spec: HotkeySpec, tx: UnboundedSender<CtlMsg>) -> Result<(), String> {
    set_spec(spec);
    EVENTS.set(tx).map_err(|_| "hook już działa".to_string())?;
    platform::spawn_hook_thread()
}

/// Matcher step shared by the platform hook. Never blocks: if the settings
/// thread holds the lock, the key passes through untouched.
#[cfg_attr(not(windows), allow(dead_code))]
fn handle_key(code: u32, down: bool, sync: impl Fn(&mut KeyMatcher)) -> bool {
    let Ok(mut guard) = MATCHER.try_lock() else { return false };
    let Some(matcher) = guard.as_mut() else { return false };
    if down {
        sync(matcher);
    }
    let decision = matcher.on_key(code, down, RECORDING.load(Ordering::Relaxed));
    if let (Some(event), Some(tx)) = (decision.event, EVENTS.get()) {
        let _ = tx.send(CtlMsg::Key((event, Instant::now())));
    }
    decision.swallow
}

#[cfg(windows)]
mod platform {
    use std::ptr::{null, null_mut};
    use std::sync::atomic::Ordering;

    use hyperrust_core::hotkey::KeyMatcher;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT,
        LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    use super::{handle_key, PAUSED};

    /// Re-read the physical key state so key-ups lost while the workstation
    /// was locked cannot leave a key stuck.
    fn resync(m: &mut KeyMatcher, current: u32) {
        // SAFETY: plain Win32 query without pointers.
        m.resync(current, |code| unsafe { GetAsyncKeyState(code as i32) } < 0);
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && !PAUSED.load(Ordering::Relaxed) {
            // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
            let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            // Our own Ctrl+V (and other injected input) must never trigger the shortcut.
            if info.flags & LLKHF_INJECTED == 0 {
                let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let vk_code = info.vkCode;
                if handle_key(vk_code, down, |m| resync(m, vk_code)) {
                    return 1;
                }
            }
        }
        // SAFETY: forwarding the unmodified hook arguments.
        unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
    }

    pub fn spawn_hook_thread() -> Result<(), String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("hyperrust-hotkey".into())
            .spawn(move || {
                // SAFETY: standard hook installation; the message loop below keeps
                // the hook alive for the lifetime of the process.
                unsafe {
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
                    let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
                    if hook.is_null() {
                        let _ = ready_tx.send(Err("SetWindowsHookExW nie powiódł się".to_string()));
                        return;
                    }
                    let _ = ready_tx.send(Ok(()));
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
                    UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())?
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn spawn_hook_thread() -> Result<(), String> {
        tracing::warn!("globalny skrót działa tylko w Windows");
        Ok(())
    }
}

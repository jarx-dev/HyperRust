//! Text insertion: clipboard + Ctrl+V, the fastest method that works in every
//! app (terminals, Electron, browsers), then the previous clipboard content is
//! put back. Our temporary text is marked so clipboard history and cloud sync
//! ignore it.

use hyperrust_core::config::PasteSettings;

/// Foreground window at the key press (`HWND` as an integer), so the text
/// lands in the app the user dictated into.
pub type Target = isize;

pub fn foreground() -> Option<Target> {
    platform::foreground()
}

pub use platform::PendingRestore;

/// Put `text` into the clipboard and (optionally) paste it. Blocking; run on a
/// worker thread. The returned restore must run under the same paste lock, so
/// the next dictation never snapshots our temporary text as "the user's".
pub fn deliver(text: &str, settings: &PasteSettings, target: Option<Target>) -> Result<Option<PendingRestore>, String> {
    platform::deliver(text, settings, target)
}

#[cfg(windows)]
mod platform {
    use std::time::{Duration, Instant};

    use clipboard_win::{options::NoClear, raw, register_format, Clipboard, EnumFormats};
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use hyperrust_core::config::PasteSettings;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
    };

    use super::Target;

    /// Formats holding GDI handles instead of HGLOBAL memory cannot be copied
    /// byte-wise; Windows re-synthesizes them from CF_DIB / CF_ENHMETAFILE peers.
    const HANDLE_FORMATS: [u32; 8] = [2, 3, 9, 14, 0x80, 0x82, 0x83, 0x8E];
    /// Don't snapshot huge clipboard contents (e.g. a copied video frame).
    const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
    const VK_V: u32 = 0x56;
    const HELD_MODIFIERS: [i32; 5] = [0x10, 0x11, 0x12, 0x5B, 0x5C]; // Shift, Ctrl, Alt, LWin, RWin

    type Snapshot = Vec<(u32, Vec<u8>)>;

    pub fn foreground() -> Option<Target> {
        // SAFETY: plain Win32 query.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_null()).then_some(hwnd as Target)
    }

    fn snapshot() -> Option<Snapshot> {
        let _clip = Clipboard::new_attempts(10).ok()?;
        let mut items = Vec::new();
        let mut total = 0usize;
        for format in EnumFormats::new() {
            if HANDLE_FORMATS.contains(&format) {
                continue;
            }
            let mut buf = Vec::new();
            if raw::get_vec(format, &mut buf).is_ok() {
                total += buf.len();
                if total > MAX_SNAPSHOT_BYTES {
                    tracing::info!("schowek za duży do zapamiętania, nie zostanie przywrócony");
                    return None;
                }
                items.push((format, buf));
            }
        }
        Some(items)
    }

    fn restore(items: &Snapshot, seq: u32) -> Result<(), String> {
        let _clip = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        // Checked while holding the clipboard: if anyone (the user, the target
        // app) changed it after our paste, their content wins.
        if raw::seq_num().map(|n| n.get()).unwrap_or(0) != seq {
            return Ok(());
        }
        raw::empty().map_err(|e| e.to_string())?;
        for (format, data) in items {
            let _ = raw::set_without_clear(*format, data);
        }
        Ok(())
    }

    fn set_text(text: &str) -> Result<u32, String> {
        let _clip = Clipboard::new_attempts(10).map_err(|e| format!("schowek zajęty: {e}"))?;
        raw::empty().map_err(|e| e.to_string())?;
        raw::set_string_with(text, NoClear).map_err(|e| format!("zapis do schowka: {e}"))?;
        // Keep dictations out of Win+V history, cloud clipboard and clipboard managers.
        if let Some(f) = register_format("ExcludeClipboardContentFromMonitorProcessing") {
            let _ = raw::set_without_clear(f.get(), &[0]);
        }
        for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
            if let Some(f) = register_format(name) {
                let _ = raw::set_without_clear(f.get(), &0u32.to_le_bytes());
            }
        }
        Ok(raw::seq_num().map(|n| n.get()).unwrap_or(0))
    }

    fn modifiers_held() -> bool {
        // SAFETY: plain Win32 queries.
        HELD_MODIFIERS.iter().any(|&vk| unsafe { GetAsyncKeyState(vk) } < 0)
    }

    /// Bring the dictation target back to the front, but only if the focus
    /// moved to one of our own windows; a window the user switched to on
    /// purpose keeps the focus.
    fn refocus(target: Target) {
        let target = target as HWND;
        // SAFETY: Win32 window queries/activation with handles checked by IsWindow.
        unsafe {
            let fg = GetForegroundWindow();
            if fg == target || IsWindow(target) == 0 {
                return;
            }
            let mut pid = 0u32;
            let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, &mut pid) };
            if !fg.is_null() && pid != GetCurrentProcessId() {
                return;
            }
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0;
            SetForegroundWindow(target);
            BringWindowToTop(target);
            if attached {
                AttachThreadInput(me, fg_thread, 0);
            }
        }
    }

    fn send_ctrl_v() -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo.key(Key::Control, Direction::Press).map_err(|e| e.to_string())?;
        let v = enigo.key(Key::Other(VK_V), Direction::Click);
        let up = enigo.key(Key::Control, Direction::Release);
        v.and(up).map_err(|e| e.to_string())
    }

    /// Previous clipboard content to put back once the target app has read
    /// the pasted text.
    pub struct PendingRestore {
        items: Snapshot,
        seq: u32,
        delay: Duration,
    }

    impl PendingRestore {
        /// Blocking: waits the configured delay, then restores.
        pub fn finish(self) {
            std::thread::sleep(self.delay);
            if let Err(e) = restore(&self.items, self.seq) {
                tracing::warn!("nie udało się przywrócić schowka: {e}");
            }
        }
    }

    pub fn deliver(text: &str, settings: &PasteSettings, target: Option<Target>) -> Result<Option<PendingRestore>, String> {
        let saved = if settings.auto_paste && settings.restore_clipboard { snapshot() } else { None };
        let seq = set_text(text)?;
        if !settings.auto_paste {
            return Ok(None);
        }

        // A still-held shortcut modifier would turn Ctrl+V into another chord.
        let deadline = Instant::now() + Duration::from_millis(800);
        while modifiers_held() {
            if Instant::now() >= deadline {
                return Err("puść klawisze Ctrl/Shift/Alt/Win — tekst jest w schowku".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(t) = target {
            refocus(t);
        }
        send_ctrl_v()?;

        Ok(saved.map(|items| PendingRestore { items, seq, delay: Duration::from_millis(settings.restore_delay_ms) }))
    }
}

#[cfg(not(windows))]
mod platform {
    use hyperrust_core::config::PasteSettings;

    use super::Target;

    pub enum PendingRestore {}

    impl PendingRestore {
        pub fn finish(self) {
            match self {}
        }
    }

    pub fn foreground() -> Option<Target> {
        None
    }

    pub fn deliver(_text: &str, _settings: &PasteSettings, _target: Option<Target>) -> Result<Option<PendingRestore>, String> {
        Err("wklejanie jest dostępne tylko w Windows".into())
    }
}

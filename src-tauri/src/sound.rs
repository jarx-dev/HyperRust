//! Start/stop cues through the Windows sound player (`PlaySoundW`), which runs
//! on its own and never touches the recording stream.

use std::sync::OnceLock;

use hyperrust_core::cue::{wav, Cue};

/// Play `cue` without waiting for it; a newer cue cuts off an older one.
pub fn play(cue: Cue) {
    // Opening the audio device on the first call can take a moment.
    tauri::async_runtime::spawn_blocking(move || play_now(cue));
}

fn play_now(cue: Cue) {
    static START: OnceLock<Vec<u8>> = OnceLock::new();
    static STOP: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = match cue {
        Cue::Start => START.get_or_init(|| wav(Cue::Start)),
        Cue::Stop => STOP.get_or_init(|| wav(Cue::Stop)),
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        // SAFETY: with SND_MEMORY the "name" is the WAV image itself; the bytes
        // are 'static, so they outlive the asynchronous playback.
        let ok = unsafe { PlaySoundW(bytes.as_ptr().cast(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
        if ok == 0 {
            tracing::warn!("sygnał {cue:?}: PlaySoundW nie odtworzył dźwięku: {}", std::io::Error::last_os_error());
        }
    }
    #[cfg(not(windows))]
    let _ = bytes;
}

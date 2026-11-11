//! Self-update from GitHub Releases or a private update server (see
//! `scripts/publish-dev.sh` and `hyperrust_core::update`). Only builds made
//! with `HYPERRUST_BUILD` set (releases, the publish script) update
//! themselves, so a local test build is never replaced under test. Newer builds are downloaded in the
//! background, verified, and swapped in only while no dictation is running:
//! the running EXE is renamed to `*.exe.old` (Windows allows renaming a running
//! image), the new one takes its place, and it is started with
//! `--after-update <pid>` so it waits for this process to exit.
//!
//! The previous build also starts itself from `*.exe.old` as a guard
//! (`--update-guard <pid>`). If the new build exits or hangs before it reports
//! readiness, the guard puts the previous EXE back, remembers the failed
//! build's SHA-256 so it is not installed again, and restarts the old version.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hyperrust_core::update::{self, Manifest};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

use crate::state::{AppState, BUILD};

pub const AFTER_UPDATE_ARG: &str = "--after-update";
const GUARD_ARG: &str = "--update-guard";
/// Wait this long after the last dictation before restarting.
const QUIET_MS: u64 = 5_000;
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(8);
/// How long the previous build waits to exit before the new one stops it.
const OLD_EXIT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a freshly installed build has to finish starting up.
const STARTUP_GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateStatus {
    /// `wyłączone` | `sprawdzanie` | `aktualna` | `pobieranie` | `gotowa` | `błąd`
    pub state: String,
    pub message: String,
    pub latest_build: Option<String>,
    pub checked_at: Option<u64>,
}

/// Words the status message when it is read, so it follows a UI language switch.
type Message = Box<dyn Fn() -> String + Send + Sync>;

#[derive(Default)]
pub struct UpdateHandle {
    status: Mutex<(UpdateStatus, Option<Message>)>,
    check_now: Notify,
}

impl UpdateHandle {
    pub fn status(&self) -> UpdateStatus {
        let guard = self.status.lock().expect("update lock");
        let (status, message) = &*guard;
        UpdateStatus { message: message.as_ref().map(|m| m()).unwrap_or_default(), ..status.clone() }
    }

    pub fn request_check(&self) {
        self.check_now.notify_one();
    }

    fn set(&self, state: &str, message: impl Fn() -> String + Send + Sync + 'static, latest: Option<&Manifest>) {
        let mut guard = self.status.lock().expect("update lock");
        let (s, text) = &mut *guard;
        s.state = state.into();
        *text = Some(Box::new(message));
        if let Some(m) = latest {
            s.latest_build = Some(m.build.clone());
        }
        s.checked_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
    }
}

/// Path where a verified update waits next to the running EXE.
fn staged_path(exe: &Path) -> PathBuf {
    exe.with_extension("update.exe")
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Written by a freshly installed build once it is up; the guard waits for it.
fn ready_marker(exe: &Path) -> PathBuf {
    exe.with_extension("update-ok")
}

/// SHA-256 of a build that failed to start; it is never installed again.
fn skip_file(exe: &Path) -> PathBuf {
    exe.with_extension("update-skip")
}

fn failed_path(exe: &Path) -> PathBuf {
    exe.with_extension("failed.exe")
}

pub async fn run(app: AppHandle) {
    let state = app.state::<AppState>();
    if env!("HYPERRUST_SELF_UPDATE").is_empty() {
        state.update.set("wyłączone", move || hyperrust_core::tr!("This build was made locally and does not update itself", "Ten build zbudowano lokalnie i nie aktualizuje się sam"), None);
        return;
    }
    let client = match update::client() {
        Ok(c) => c,
        Err(e) => {
            state.update.set("błąd", move || hyperrust_core::tr!("HTTP client: {e}", "Klient HTTP: {e}"), None);
            return;
        }
    };
    tokio::select! {
        _ = tokio::time::sleep(FIRST_CHECK_DELAY) => {}
        _ = state.update.check_now.notified() => {}
    }
    loop {
        let settings = state.settings().updates;
        if settings.enabled {
            if let Some((staged, manifest)) = check_and_download(&app, &client).await {
                if lock_when_idle(&app).await {
                    match apply(&staged) {
                        Ok(()) => {
                            tracing::info!("aktualizacja {} zainstalowana, restartuję", manifest.build);
                            app.exit(0);
                            return;
                        }
                        Err(e) => {
                            state.end_install();
                            tracing::warn!("instalacja aktualizacji: {e}");
                            state.update.set("błąd", move || hyperrust_core::tr!("Installation: {e}", "Instalacja: {e}"), None);
                        }
                    }
                } else {
                    state.update.set("wyłączone", move || hyperrust_core::tr!("Automatic updates are off", "Automatyczne aktualizacje są wyłączone"), None);
                }
            }
        } else {
            state.update.set("wyłączone", move || hyperrust_core::tr!("Automatic updates are off", "Automatyczne aktualizacje są wyłączone"), None);
        }
        let interval = Duration::from_secs(settings.interval_s.max(15));
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = state.update.check_now.notified() => {}
        }
    }
}

/// Returns the path of a verified, staged EXE when a newer build exists.
async fn check_and_download(app: &AppHandle, client: &reqwest::Client) -> Option<(PathBuf, Manifest)> {
    let state = app.state::<AppState>();
    let settings = state.settings().updates;
    let url = match update::validate_url(&settings.url) {
        Ok(u) => u,
        Err(e) => {
            state.update.set("błąd", move || e.to_string(), None);
            return None;
        }
    };
    state.update.set("sprawdzanie", move || hyperrust_core::tr!("Checking…", "Sprawdzanie…"), None);
    let manifest = match update::fetch_manifest(client, &url).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("sprawdzanie aktualizacji: {e}");
            state.update.set("błąd", move || hyperrust_core::tr!("Update server: {e}", "Serwer aktualizacji: {e}"), None);
            return None;
        }
    };
    if !manifest.is_newer_than(&BUILD) {
        let build = BUILD.build;
        state.update.set("aktualna", move || hyperrust_core::tr!("Up to date ({build})", "Najnowsza wersja ({build})"), Some(&manifest));
        return None;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            state.update.set("błąd", move || hyperrust_core::tr!("Program path: {e}", "Ścieżka programu: {e}"), Some(&manifest));
            return None;
        }
    };
    let skipped = std::fs::read_to_string(skip_file(&exe)).unwrap_or_default();
    if skipped.trim() == manifest.sha256 {
        let build = manifest.build.clone();
        state.update.set(
            "błąd",
            move || hyperrust_core::tr!(
                "Build {build} failed to start and was rolled back — publish a newer one",
                "Build {build} nie uruchomił się i został wycofany — opublikuj nowszy"
            ),
            Some(&manifest),
        );
        return None;
    }
    let staged = staged_path(&exe);
    let already = std::fs::read(&staged).ok().is_some_and(|b| update::verify(&b, &manifest).is_ok());
    if !already {
        let build = manifest.build.clone();
        state.update.set("pobieranie", move || hyperrust_core::tr!("Downloading {build}…", "Pobieranie {build}…"), Some(&manifest));
        let bytes = match update::download_url(&url, &manifest) {
            Ok(dl) => update::download_verified(client, &dl, &manifest).await,
            Err(e) => Err(e),
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => {
                state.update.set("błąd", move || hyperrust_core::tr!("Download: {e}", "Pobieranie: {e}"), Some(&manifest));
                return None;
            }
        };
        if let Err(e) = update::write_atomically(&staged, &bytes) {
            let path = staged.display().to_string();
            state.update.set("błąd", move || hyperrust_core::tr!("Writing {path}: {e}", "Zapis {path}: {e}"), Some(&manifest));
            return None;
        }
    }
    tracing::info!("aktualizacja {} gotowa ({})", manifest.build, manifest.notes);
    let build = manifest.build.clone();
    state.update.set("gotowa", move || hyperrust_core::tr!("Will install {build} after the dictation ends", "Zainstaluję {build} po zakończeniu dyktowania"), Some(&manifest));
    Some((staged, manifest))
}

/// Wait until no dictation runs, then block new ones for the swap. Returns
/// `false` if updates were switched off meanwhile.
async fn lock_when_idle(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    loop {
        if !state.settings().updates.enabled {
            return false;
        }
        if state.try_begin_install(QUIET_MS) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Swap the EXE, start the new build and its guard. On error everything is
/// rolled back.
fn apply(staged: &Path) -> Result<(), String> {
    if !cfg!(windows) {
        return Err(hyperrust_core::tr!("replacing the program works only on Windows", "podmiana programu działa tylko w Windows"));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(ready_marker(&exe));
    std::fs::rename(&exe, &old).map_err(|e| hyperrust_core::tr!("renaming the running program: {e}", "zmiana nazwy bieżącego programu: {e}"))?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(hyperrust_core::tr!("replacing the program: {e}", "podmiana programu: {e}"));
    }
    let mut child = match std::process::Command::new(&exe)
        .arg(AFTER_UPDATE_ARG)
        .arg(std::process::id().to_string())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::rename(&exe, staged);
            let _ = std::fs::rename(&old, &exe);
            return Err(hyperrust_core::tr!("starting the new version: {e}", "uruchomienie nowej wersji: {e}"));
        }
    };
    // Our own image now lives at `old`; run it as the guard of the new build.
    // Without a guard a broken build could not be rolled back, so the
    // install is abandoned (the new process is still waiting for us to exit).
    if let Err(e) = std::process::Command::new(&old)
        .arg(GUARD_ARG)
        .arg(child.id().to_string())
        .arg(std::process::id().to_string())
        .spawn()
    {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::rename(&exe, staged);
        let _ = std::fs::rename(&old, &exe);
        return Err(hyperrust_core::tr!("starting the update guard: {e}", "uruchomienie strażnika aktualizacji: {e}"));
    }
    Ok(())
}

/// Entry point of `--update-guard <new-pid> <previous-pid>`, run from the
/// previous build's `*.exe.old`. Returns `true` when this process was the
/// guard and must exit.
pub fn run_guard(args: &[String]) -> bool {
    let Some(pos) = args.iter().position(|a| a == GUARD_ARG) else { return false };
    let pid = |i: usize| args.get(pos + i).and_then(|p| p.parse::<u32>().ok());
    let (Some(new_pid), Some(previous_pid)) = (pid(1), pid(2)) else { return true };
    let Ok(old) = std::env::current_exe() else { return true };
    let exe = old.with_extension(""); // HyperRust.exe.old -> HyperRust.exe
    let marker = ready_marker(&exe);
    match platform::watch_startup(new_pid, || marker.exists(), STARTUP_GRACE) {
        platform::Startup::Ready => {
            let _ = std::fs::remove_file(&marker);
        }
        outcome => {
            tracing::error!("nowa wersja nie wystartowała ({outcome:?}), przywracam poprzednią");
            // The restored build must not meet the previous process still
            // holding the single-instance lock, or both end up gone.
            platform::wait_for_exit(previous_pid, OLD_EXIT_TIMEOUT);
            if let Err(e) = roll_back(&exe, &old) {
                tracing::error!("wycofanie aktualizacji: {e}");
            }
        }
    }
    true
}

fn roll_back(exe: &Path, old: &Path) -> Result<(), String> {
    if let Ok(bytes) = std::fs::read(exe) {
        let _ = std::fs::write(skip_file(exe), update::sha256_hex(&bytes));
    }
    let failed = failed_path(exe);
    let _ = std::fs::remove_file(&failed);
    std::fs::rename(exe, &failed).map_err(|e| format!("odsunięcie nieudanej wersji: {e}"))?;
    std::fs::rename(old, exe).map_err(|e| format!("przywrócenie poprzedniej wersji: {e}"))?;
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| format!("uruchomienie poprzedniej wersji: {e}"))?;
    Ok(())
}

/// Called first thing in `main` of a freshly installed build: wait until the
/// previous process (which still owns the single-instance lock) has exited.
pub fn finish_pending_update(args: &[String]) -> bool {
    let Some(pos) = args.iter().position(|a| a == AFTER_UPDATE_ARG) else { return false };
    if let Some(pid) = args.get(pos + 1).and_then(|p| p.parse::<u32>().ok()) {
        platform::wait_for_exit(pid, OLD_EXIT_TIMEOUT);
    }
    true
}

/// The app is up: tell the guard, forget a previously failed build and remove
/// the leftovers of the swap once the guard has released them.
pub fn startup_complete(after_update: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    if after_update {
        if let Err(e) = std::fs::write(ready_marker(&exe), BUILD.build) {
            tracing::warn!("znacznik gotowości aktualizacji: {e}");
        }
        let _ = std::fs::remove_file(skip_file(&exe));
    }
    let leftovers: Vec<PathBuf> = [old_path(&exe), failed_path(&exe)].into_iter().filter(|p| p.exists()).collect();
    if leftovers.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if leftovers.iter().all(|p| std::fs::remove_file(p).is_ok() || !p.exists()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

#[cfg(windows)]
mod platform {
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    #[derive(Debug)]
    pub enum Startup {
        Ready,
        Exited,
        TimedOut,
    }

    /// Our own builds only: a recycled PID must never be terminated. The image
    /// must be exactly our EXE or its `*.exe.old` (the previous build).
    fn is_hyperrust(handle: HANDLE) -> bool {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buffer and length describe valid writable memory.
        if unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
            return false;
        }
        let image = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        let Ok(current) = std::env::current_exe() else { return false };
        let current = current.to_string_lossy().to_lowercase();
        let exe = current.strip_suffix(".old").unwrap_or(&current);
        image == exe || image == format!("{exe}.old")
    }

    fn open(pid: u32) -> Option<HANDLE> {
        let access = PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: plain Win32 call; the handle is checked for null.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(handle)
    }

    pub fn wait_for_exit(pid: u32, timeout: Duration) {
        let Some(handle) = open(pid) else { return }; // already gone
        // SAFETY: valid process handle, closed below.
        unsafe {
            if WaitForSingleObject(handle, timeout.as_millis() as u32) == WAIT_TIMEOUT && is_hyperrust(handle) {
                // The previous build ignored its exit; it would keep the
                // single-instance lock and swallow this start.
                TerminateProcess(handle, 1);
                WaitForSingleObject(handle, 5_000);
            }
            CloseHandle(handle);
        }
    }

    pub fn watch_startup(pid: u32, ready: impl Fn() -> bool, grace: Duration) -> Startup {
        let Some(handle) = open(pid) else {
            return if ready() { Startup::Ready } else { Startup::Exited };
        };
        let deadline = Instant::now() + grace;
        let outcome = loop {
            if ready() {
                break Startup::Ready;
            }
            // SAFETY: valid process handle.
            if unsafe { WaitForSingleObject(handle, 200) } == WAIT_OBJECT_0 {
                break if ready() { Startup::Ready } else { Startup::Exited };
            }
            if Instant::now() >= deadline {
                // SAFETY: valid process handle opened with PROCESS_TERMINATE.
                unsafe {
                    if is_hyperrust(handle) {
                        TerminateProcess(handle, 1);
                        WaitForSingleObject(handle, 5_000);
                    }
                }
                break Startup::TimedOut;
            }
        };
        // SAFETY: valid handle, closed once.
        unsafe { CloseHandle(handle) };
        outcome
    }
}

#[cfg(not(windows))]
mod platform {
    use std::time::Duration;

    #[derive(Debug)]
    pub enum Startup {
        Ready,
        #[allow(dead_code)]
        Exited,
    }

    pub fn wait_for_exit(_pid: u32, _timeout: Duration) {}

    pub fn watch_startup(_pid: u32, _ready: impl Fn() -> bool, _grace: Duration) -> Startup {
        Startup::Ready
    }
}

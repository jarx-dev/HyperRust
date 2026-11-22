//! Anonymous crash reports (README, "Crash reports"). After a panic in a release
//! build, one report per process goes to the author's Rustrak server
//! (errors.jarx.pl, Sentry protocol) unless "Send crash reports" is off.
//!
//! The report holds exactly three facts: the app version (`hyperrust@<build>`,
//! the build id the updater compares), the Windows version (`10.0.<build>`) and
//! the place of the panic in the source code (`file:line`, see [`short_path`]).
//! Never sent: the panic message (it may quote a dictation, a file or a key),
//! file names, machine or user names, settings, logs, environment, any id of the
//! installation or the user. The event id is random for each report.

use std::hash::{BuildHasher, Hasher};
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::state::BUILD;

/// The public ingest key of Rustrak project 11 (`hyperrust`); not a secret.
const ENDPOINT: &str = "https://errors.jarx.pl/api/11/envelope/";
const KEY: &str = "9f9251f801e54344b529c3338b8d2683";
const CLIENT: &str = concat!("hyperrust/", env!("HYPERRUST_BUILD"));

/// Mirrors `Settings::crash_reports`; set at start-up and on every save.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// At most one report per process, so a cascade of panics sends one.
static SENT: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Report first, then run the hook that was there before (the default one).
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        previous(info);
    }));
}

fn report(info: &PanicHookInfo<'_>) {
    if cfg!(debug_assertions) || !ENABLED.load(Ordering::Relaxed) || SENT.swap(true, Ordering::SeqCst) {
        return;
    }
    let place = info.location().map(|l| format!("{}:{}", short_path(l.file()), l.line())).unwrap_or_else(|| "unknown".into());
    let body = envelope(&event_id(), BUILD.build, &windows_version(), &place);
    // Own thread and runtime: the panicking thread may sit inside a tokio runtime.
    // The hook waits at most ~3 s, even if DNS hangs; a failed spawn skips the report.
    let (done, wait) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().spawn(move || {
        send(body);
        let _ = done.send(());
    });
    if spawned.is_ok() {
        let _ = wait.recv_timeout(Duration::from_secs(3));
    }
}

fn send(body: String) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
    };
    hyperrust_core::http::install_crypto_provider();
    rt.block_on(async move {
        let Ok(client) = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .timeout(Duration::from_secs(3))
            .user_agent(CLIENT) // Cloudflare rejects library default user agents (error 1010).
            .build()
        else {
            return;
        };
        let auth = format!("Sentry sentry_version=7, sentry_key={KEY}, sentry_client={CLIENT}");
        let _ = client
            .post(ENDPOINT)
            .header("Content-Type", "application/x-sentry-envelope")
            .header("X-Sentry-Auth", auth)
            .body(body)
            .send()
            .await;
    });
    // Don't wait for a resolver thread still stuck on DNS.
    rt.shutdown_background();
}

/// The whole report: a Sentry envelope of three JSON lines.
fn envelope(event_id: &str, build: &str, windows: &str, place: &str) -> String {
    let event = serde_json::json!({
        "event_id": event_id,
        "level": "fatal",
        "platform": "native",
        "release": format!("hyperrust@{build}"),
        "environment": "production",
        "contexts": {"os": {"name": "Windows", "version": windows}},
        "exception": {"values": [{
            "type": "panic",
            "value": place,
            "mechanism": {"type": "panic", "handled": false},
        }]},
        "fingerprint": [place],
    });
    format!("{}\n{}\n{event}\n", serde_json::json!({"event_id": event_id}), serde_json::json!({"type": "event"}))
}

/// A panic location without the build machine's directories. Workspace paths
/// (`src/lib.rs`) stay; absolute ones keep only the part that names no local
/// directory: `<crate>/src/...` from the cargo registry or a git checkout,
/// `core/src/...` from the standard library, otherwise the file name alone.
fn short_path(file: &str) -> String {
    let file = file.replace('\\', "/");
    let absolute = file.starts_with('/') || file.as_bytes().get(1) == Some(&b':');
    if !absolute && !file.split('/').any(|c| c == "..") {
        return file;
    }
    let after = |marker: &str| file.rfind(marker).map(|i| &file[i + marker.len()..]);
    // Registry: `<index>/<name>-<version>/...`; git: `<repo>/<hex rev>/...`. Kept only
    // when the shape matches, so a look-alike directory never leaks a local name.
    let tail = |rest: &str, looks_right: fn(&str) -> bool| {
        let (_, tail) = rest.split_once('/')?;
        looks_right(tail.split('/').next()?).then(|| tail.to_string())
    };
    if let Some(t) = after("/registry/src/").and_then(|r| tail(r, is_crate_dir)) {
        return t;
    }
    if let Some(t) = after("/git/checkouts/").and_then(|r| tail(r, |c| c.len() >= 7 && c.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))) {
        return t;
    }
    if let Some(i) = file.find("/rustc/") {
        if let Some(j) = file[i..].find("/library/") {
            return file[i + j + "/library/".len()..].to_string();
        }
    }
    file.rsplit('/').next().unwrap_or_default().to_string()
}

/// `<name>-<digit>...`, like `ureq-2.12.1`.
fn is_crate_dir(c: &str) -> bool {
    c.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        && c.match_indices('-').any(|(i, _)| i > 0 && c.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit))
}

/// 32 hex digits from std's randomly seeded hasher; no extra dependency.
fn event_id() -> String {
    let half = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

#[cfg(windows)]
fn windows_version() -> String {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion") else {
        return "unknown".into();
    };
    let major: Result<u32, _> = key.get_value("CurrentMajorVersionNumber");
    let minor: Result<u32, _> = key.get_value("CurrentMinorVersionNumber");
    let build: Result<String, _> = key.get_value("CurrentBuildNumber");
    // A number only, so nothing else from the registry can reach the report.
    match (major, minor, build.ok().and_then(|b| b.trim().parse::<u32>().ok())) {
        (Ok(major), Ok(minor), Some(build)) => format!("{major}.{minor}.{build}"),
        _ => "unknown".into(),
    }
}

#[cfg(not(windows))]
fn windows_version() -> String {
    "unknown".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_only_the_three_facts() {
        let id = event_id();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        let place = format!("{}:42", short_path("/home/someone/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/reqwest-0.13.1/src/async_impl/client.rs"));
        let body = envelope(&id, "abc1234-1760000000", "10.0.26100", &place);

        assert!(body.ends_with('\n'));
        let lines: Vec<serde_json::Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], serde_json::json!({"event_id": id}));
        assert_eq!(lines[1], serde_json::json!({"type": "event"}));
        let e = &lines[2];
        assert_eq!(e["release"], "hyperrust@abc1234-1760000000");
        assert_eq!(e["level"], "fatal");
        assert_eq!(e["contexts"]["os"]["version"], "10.0.26100");
        assert_eq!(e["exception"]["values"][0]["value"], "reqwest-0.13.1/src/async_impl/client.rs:42");
        assert_eq!(e["fingerprint"][0], "reqwest-0.13.1/src/async_impl/client.rs:42");
        let mut keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["contexts", "environment", "event_id", "exception", "fingerprint", "level", "platform", "release"]);
        assert!(!body.contains("someone") && !body.contains("home"));
    }

    #[test]
    fn short_path_drops_build_machine_directories() {
        assert_eq!(short_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(short_path("crates/core/src/llm.rs"), "crates/core/src/llm.rs");
        assert_eq!(
            short_path(r"C:\Users\runneradmin\.cargo\registry\src\index.crates.io-6f17d22bba15001f\tokio-1.48.0\src\runtime\park.rs"),
            "tokio-1.48.0/src/runtime/park.rs"
        );
        assert_eq!(short_path("/home/someone/.cargo/git/checkouts/tauri-0123abcd/4f5e6d7/core/src/app.rs"), "4f5e6d7/core/src/app.rs");
        assert_eq!(short_path("/rustc/abc123/library/core/src/option.rs"), "core/src/option.rs");
        assert_eq!(short_path(r"C:\Users\someone\x.rs"), "x.rs");
        assert_eq!(short_path("/home/demo/src/gen.rs"), "gen.rs");
        assert_eq!(short_path("../../x/src/a.rs"), "a.rs");
        assert_eq!(short_path(r"\\?\UNC\buildhost\registry\src\agents\alice\work\main.rs"), "main.rs");
        assert_eq!(
            short_path("D:/registry/src/Users/jan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ureq-2.12.1/src/pool.rs"),
            "ureq-2.12.1/src/pool.rs"
        );
        assert_eq!(short_path("/home/someone/.cargo/git/checkouts/tauri-0123abcd/main/core/src/app.rs"), "app.rs");
    }

    #[test]
    fn this_file_reports_as_a_workspace_path() {
        let body = envelope(&event_id(), "b", "10.0.1", &format!("{}:{}", short_path(file!()), line!()));
        assert!(body.contains("\"src/crash_report.rs:"));
    }
}

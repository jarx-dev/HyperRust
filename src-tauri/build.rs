//! Embeds the build identity used by the self-updater. `scripts/publish-dev.sh`
//! and the release workflow set both variables so the EXE and its `latest.json`
//! agree; plain builds fall back to the git commit and the current time and
//! never update themselves (`HYPERRUST_SELF_UPDATE` stays empty).

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-env-changed=HYPERRUST_BUILD");
    println!("cargo:rerun-if-env-changed=HYPERRUST_BUILT_AT");

    let built_at = std::env::var("HYPERRUST_BUILT_AT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let release = std::env::var("HYPERRUST_BUILD").ok().filter(|v| !v.trim().is_empty());
    let self_update = if release.is_some() { "1" } else { "" };
    let build = release.unwrap_or_else(|| format!("{}-{built_at}", git_short_sha().unwrap_or_else(|| "local".into())));
    println!("cargo:rustc-env=HYPERRUST_BUILD={build}");
    println!("cargo:rustc-env=HYPERRUST_SELF_UPDATE={self_update}");
    println!("cargo:rustc-env=HYPERRUST_BUILT_AT={built_at}");

    tauri_build::build()
}

fn git_short_sha() -> Option<String> {
    let out = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok()?;
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !sha.is_empty()).then_some(sha)
}

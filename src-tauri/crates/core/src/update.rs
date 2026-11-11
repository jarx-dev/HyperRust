//! Self-update. Public builds read `latest.json` from GitHub Releases
//! (`.github/workflows/release.yml`); a private build gets its own server's
//! address at build time (`HYPERRUST_DEFAULT_UPDATE_URL`, see
//! `scripts/publish-dev.sh`). The app polls the manifest, downloads a newer
//! build and verifies it here. Replacing the running EXE is the app's job.
//!
//! Plain HTTP is accepted only for loopback and Tailscale addresses, where
//! WireGuard already authenticates and encrypts the peer; anything else must be
//! HTTPS. The SHA-256 from the manifest guards against truncated downloads.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::error::{Error, Result};
use crate::tr;

/// Where public releases are published; GitHub publishes no model ranking.
pub const GITHUB_UPDATE_URL: &str = "https://github.com/jarx-dev/HyperRust/releases/latest/download/latest.json";
/// The manifest a new installation polls: a private update server baked in at
/// build time, otherwise GitHub Releases.
pub const DEFAULT_UPDATE_URL: &str = match option_env!("HYPERRUST_DEFAULT_UPDATE_URL") {
    Some(url) => url,
    None => GITHUB_UPDATE_URL,
};
/// Refuse anything larger than this (the release EXE is ~10 MB).
pub const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// HTTP client for update checks and downloads. Redirects are followed only
/// from HTTPS to HTTPS (GitHub's `releases/latest/download` redirects twice):
/// from the tailnet one could lead to plain HTTP on the internet, where the
/// manifest and its SHA-256 could be swapped together.
pub fn client() -> reqwest::Result<Client> {
    crate::http::install_crypto_provider();
    Client::builder()
        .connect_timeout(crate::http::CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let https = |u: &Url| u.scheme() == "https";
            if attempt.previous().len() > 5 {
                attempt.error(tr!("too many redirects", "za dużo przekierowań"))
            } else if https(attempt.url()) && attempt.previous().iter().all(https) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("HyperRust/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// `latest.json` written by the publish script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// Unique build id, e.g. `a1b2c3d-dirty-1760512345`.
    pub build: String,
    /// Unix seconds; only strictly newer builds are installed (no downgrades).
    pub built_at: u64,
    /// Hex SHA-256 of the EXE.
    pub sha256: String,
    /// EXE location, relative to the manifest URL or absolute.
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
    /// Free text, e.g. the commit subject.
    #[serde(default)]
    pub notes: String,
}

/// Identity of the running binary, embedded at compile time by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build: &'static str,
    pub built_at: u64,
}

impl Manifest {
    pub fn is_newer_than(&self, current: &BuildInfo) -> bool {
        self.built_at > current.built_at && self.build != current.build
    }
}

/// Parse and sanity-check `latest.json`.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    let what = tr!("update manifest", "manifest aktualizacji");
    let mut m: Manifest = serde_json::from_slice(bytes).map_err(|e| Error::InvalidResponse(format!("{what}: {e}")))?;
    m.sha256 = m.sha256.trim().to_ascii_lowercase();
    if m.sha256.len() != 64 || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::InvalidResponse(tr!("{what}: bad sha256 sum", "{what}: zła suma sha256")));
    }
    if m.build.trim().is_empty() || m.url.trim().is_empty() {
        return Err(Error::InvalidResponse(tr!("{what}: missing build or url field", "{what}: brak pola build lub url")));
    }
    Ok(m)
}

/// Accept `https://` anywhere, `http://` only for loopback and the tailnet.
pub fn validate_url(raw: &str) -> Result<Url> {
    validate_private_url(raw, &tr!("update", "aktualizacji"))
}

/// The transport rule of [`validate_url`] for any HyperRust server; `what`
/// names the address in the error message (`tr!("history", "historii")`).
pub fn validate_private_url(raw: &str, what: &str) -> Result<Url> {
    let url = Url::parse(raw.trim()).map_err(|e| Error::Other(tr!("{what} address \"{raw}\": {e}", "adres {what} „{raw}”: {e}")))?;
    let ok = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => url.host().is_some_and(is_private_transport_host),
        _ => false,
    };
    if ok {
        Ok(url)
    } else {
        Err(Error::Other(tr!(
            "{what} address \"{raw}\" must be https://, or http:// on the Tailscale network (100.64.0.0/10, *.ts.net) or local",
            "adres {what} „{raw}” musi być https:// albo http:// w sieci Tailscale (100.64.0.0/10, *.ts.net) lub lokalny"
        )))
    }
}

fn is_private_transport_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".ts.net")
        }
        Host::Ipv4(ip) => ip.is_loopback() || is_tailscale_v4(ip),
        Host::Ipv6(ip) => ip.is_loopback() || is_tailscale_v6(ip),
    }
}

/// Tailscale's CGNAT range 100.64.0.0/10.
fn is_tailscale_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (b & 0xC0) == 64
}

/// Tailscale's ULA prefix fd7a:115c:a1e0::/48.
fn is_tailscale_v6(ip: Ipv6Addr) -> bool {
    ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
}

/// Absolute, validated download URL of the manifest's EXE.
pub fn download_url(manifest_url: &Url, m: &Manifest) -> Result<Url> {
    let joined = manifest_url
        .join(m.url.trim())
        .map_err(|e| Error::InvalidResponse(tr!("update manifest: url: {e}", "manifest aktualizacji: url: {e}")))?;
    validate_url(joined.as_str())
}

pub async fn fetch_manifest(client: &Client, url: &Url) -> Result<Manifest> {
    let resp = client
        .get(url.clone())
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .timeout(MANIFEST_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        return Err(Error::from_status(status.as_u16(), &String::from_utf8_lossy(&body)));
    }
    parse_manifest(&body)
}

/// Download the manifest's EXE into memory and verify size, SHA-256 and the PE
/// header. Returns the verified bytes.
pub async fn download_verified(client: &Client, url: &Url, m: &Manifest) -> Result<Vec<u8>> {
    let mut resp = client.get(url.clone()).timeout(DOWNLOAD_TIMEOUT).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::from_status(status.as_u16(), &body));
    }
    let expected = m.size.or(resp.content_length()).unwrap_or(0);
    if expected > MAX_DOWNLOAD_BYTES {
        return Err(Error::InvalidResponse(tr!("update too large ({expected} B)", "aktualizacja za duża ({expected} B)")));
    }
    let mut bytes = Vec::with_capacity(expected as usize);
    while let Some(chunk) = resp.chunk().await? {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_DOWNLOAD_BYTES {
            return Err(Error::InvalidResponse(tr!("update exceeds the size limit", "aktualizacja przekracza limit rozmiaru")));
        }
        bytes.extend_from_slice(&chunk);
    }
    verify(&bytes, m)?;
    Ok(bytes)
}

/// Check a downloaded EXE against its manifest.
pub fn verify(bytes: &[u8], m: &Manifest) -> Result<()> {
    if let Some(size) = m.size {
        let got = bytes.len();
        if got as u64 != size {
            return Err(Error::InvalidResponse(tr!(
                "update: size {got} B instead of {size} B",
                "aktualizacja: rozmiar {got} B zamiast {size} B"
            )));
        }
    }
    let actual = sha256_hex(bytes);
    if actual != m.sha256 {
        return Err(Error::InvalidResponse(tr!(
            "update: sha256 sum {actual} does not match the manifest",
            "aktualizacja: suma sha256 {actual} nie zgadza się z manifestem"
        )));
    }
    if !bytes.starts_with(b"MZ") {
        return Err(Error::InvalidResponse(tr!("update is not an EXE file", "aktualizacja nie jest plikiem EXE")));
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Write `bytes` to `path` via a sibling temp file, so a crash never leaves a
/// half-written file under the final name.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("part");
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(bytes: &[u8]) -> Manifest {
        Manifest {
            version: "0.1.0".into(),
            build: "abc1234-1760000100".into(),
            built_at: 1_760_000_100,
            sha256: sha256_hex(bytes),
            url: "HyperRust-abc1234-1760000100.exe".into(),
            size: Some(bytes.len() as u64),
            notes: String::new(),
        }
    }

    const CURRENT: BuildInfo = BuildInfo { version: "0.1.0", build: "old-1760000000", built_at: 1_760_000_000 };

    #[test]
    fn parses_publish_script_output() {
        let json = br#"{"version":"0.1.0","build":"a1b2c3d-1760512345","built_at":1760512345,
            "sha256":"E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
            "url":"HyperRust-a1b2c3d-1760512345.exe","size":0,"notes":"feat: x"}"#;
        let m = parse_manifest(json).unwrap();
        assert_eq!(m.sha256, sha256_hex(b""));
        assert_eq!(m.notes, "feat: x");
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(parse_manifest(b"{}").is_err());
        assert!(parse_manifest(br#"{"version":"1","build":"b","built_at":1,"sha256":"zz","url":"x"}"#).is_err());
        let no_build = format!(r#"{{"version":"1","build":" ","built_at":1,"sha256":"{}","url":"x"}}"#, sha256_hex(b""));
        assert!(parse_manifest(no_build.as_bytes()).is_err());
    }

    #[test]
    fn only_strictly_newer_builds_install() {
        let mut m = manifest(b"MZ");
        assert!(m.is_newer_than(&CURRENT));
        m.built_at = CURRENT.built_at;
        assert!(!m.is_newer_than(&CURRENT), "same time");
        m.built_at = CURRENT.built_at - 1;
        assert!(!m.is_newer_than(&CURRENT), "downgrade");
        m.built_at = CURRENT.built_at + 1;
        m.build = CURRENT.build.into();
        assert!(!m.is_newer_than(&CURRENT), "same build");
    }

    #[test]
    fn http_only_inside_the_tailnet() {
        for ok in [
            GITHUB_UPDATE_URL,
            "http://100.101.102.103:8790/latest.json",
            "http://100.127.255.1/latest.json",
            "http://server.example.ts.net:8790/latest.json",
            "http://localhost:8790/latest.json",
            "http://127.0.0.1/latest.json",
            "http://[fd7a:115c:a1e0::1]/latest.json",
            "https://example.com/latest.json",
        ] {
            assert!(validate_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/latest.json",
            "http://100.128.0.1/latest.json", // outside 100.64.0.0/10
            "http://100.63.255.255/latest.json",
            "http://192.168.0.10/latest.json",
            "http://evil.ts.net.example.com/latest.json",
            "ftp://100.101.102.103/latest.json",
            "file:///C:/latest.json",
            "nonsense",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exe_url_is_resolved_and_validated() {
        let base = validate_url("http://100.101.102.103:8790/latest.json").unwrap();
        let mut m = manifest(b"MZ");
        assert_eq!(
            download_url(&base, &m).unwrap().as_str(),
            "http://100.101.102.103:8790/HyperRust-abc1234-1760000100.exe"
        );
        m.url = "http://example.com/x.exe".into();
        assert!(download_url(&base, &m).is_err(), "manifest must not redirect to plain HTTP outside the tailnet");
    }

    #[test]
    fn verification_checks_hash_size_and_header() {
        let exe = b"MZ\x90\x00rest";
        let m = manifest(exe);
        assert!(verify(exe, &m).is_ok());
        assert!(verify(b"MZ\x90\x00resT", &m).is_err(), "hash");
        assert!(verify(b"MZ", &m).is_err(), "size");
        let not_exe = b"<html>";
        assert!(verify(not_exe, &manifest(not_exe)).is_err(), "header");
    }

    #[test]
    fn atomic_write_replaces_target() {
        let dir = std::env::temp_dir().join(format!("hyperrust-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("HyperRust.update.exe");
        write_atomically(&target, b"one").unwrap();
        write_atomically(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert!(!target.with_extension("part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 302 Found\r\nLocation: http://example.com/latest.json\r\nContent-Length: 0\r\n\r\n";
            sock.write_all(resp.as_bytes()).await.unwrap();
        });
        let url = validate_url(&format!("http://{addr}/latest.json")).unwrap();
        let err = fetch_manifest(&client().unwrap(), &url).await.unwrap_err();
        assert!(err.to_string().contains("302"), "{err}");
    }
}

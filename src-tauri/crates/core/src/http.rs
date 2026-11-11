//! Shared HTTP client and connection warm-up.

use std::time::Duration;

use reqwest::Client;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Install the process-wide rustls crypto provider used by the WebSocket client
/// (reqwest configures its own). Safe to call more than once.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// One pooled client for every provider: keeps TLS connections alive between
/// dictations so the request after a key release skips the handshake.
pub fn build_client() -> reqwest::Result<Client> {
    install_crypto_provider();
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(120))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .user_agent(concat!("HyperRust/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Open (or refresh) a pooled connection to `url`. Any HTTP status counts as
/// success: only the TCP/TLS connection matters. Returns the round-trip time.
pub async fn warm_up(client: &Client, url: &str) -> Result<Duration, String> {
    let start = std::time::Instant::now();
    client
        .head(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(start.elapsed())
}

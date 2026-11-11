//! Prints the ElevenLabs and OpenRouter balances the settings window shows,
//! then the tray tooltip built from them.
//!
//! ```text
//! ELEVENLABS_API_KEY=… OPENROUTER_API_KEY=… cargo run -p hyperrust-core --example balance
//! ```
//! Keys come from environment variables only (see `SecretKey::env_var`).

use hyperrust_core::balance;
use hyperrust_core::config::SecretKey;
use hyperrust_core::http::build_client;

fn env_key(k: SecretKey) -> Option<String> {
    std::env::var(k.env_var()).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() {
    let client = build_client().expect("klient HTTP");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("zegar")
        .as_secs();
    let mut lines = vec!["HyperRust — tryb: Prompt dla agenta".to_string()];
    match env_key(SecretKey::ElevenLabs) {
        Some(key) => {
            let b = balance::elevenlabs(&client, &key, now).await;
            println!("ElevenLabs: {b:?}");
            if let Ok(b) = b {
                lines.push(balance::elevenlabs_line(&b, balance::SCRIBE_REALTIME_CREDITS_PER_MIN, now));
            }
        }
        None => println!("ElevenLabs: brak klucza"),
    }
    match env_key(SecretKey::OpenRouter) {
        Some(key) => {
            let b = balance::openrouter(&client, &key).await;
            println!("OpenRouter: {b:?}");
            if let Ok(b) = b {
                lines.push(balance::openrouter_line(&b));
            }
        }
        None => println!("OpenRouter: brak klucza"),
    }
    let optional = ["Skrót: Ctrl+Shift+Space (przytrzymaj lub stuknij dwa razy)".to_string()];
    println!("\nDymek w zasobniku:\n{}", balance::fit_tooltip(&lines, &optional, balance::TOOLTIP_MAX));
}

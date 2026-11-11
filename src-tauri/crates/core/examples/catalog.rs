//! Prints the speech-to-text model lists of the settings pickers and the
//! "new models" the overview shows, from the real catalogs.
//!
//! ```text
//! OPENAI_API_KEY=… CLIPROXY_API_KEY=… cargo run -p hyperrust-core --example catalog [-- <llm-base-url> <llm-model>]
//! ```
//! Keys come from environment variables only (see `SecretKey::env_var`).

use hyperrust_core::catalog::{self, CatalogModel, Price};
use hyperrust_core::config::SecretKey;
use hyperrust_core::http::build_client;
use hyperrust_core::stt::SttProviderKind;

fn env_key(k: SecretKey) -> Option<String> {
    std::env::var(k.env_var()).ok().filter(|v| !v.trim().is_empty())
}

fn ids(models: &[CatalogModel]) -> String {
    let price = |m: &CatalogModel| match m.price {
        Some(Price::Hour { usd }) => format!(" ({usd:.3} $/h)"),
        Some(Price::Tokens { input, output }) => format!(" ({input:.2}/{output:.2} $ za 1M tokenów)"),
        None => String::new(),
    };
    models.iter().map(|m| format!("{}{}", m.id, price(m))).collect::<Vec<_>>().join(", ")
}

#[tokio::main]
async fn main() {
    let client = build_client().expect("klient HTTP");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("zegar")
        .as_secs();
    for provider in [SttProviderKind::ElevenLabs, SttProviderKind::Xai, SttProviderKind::OpenAi, SttProviderKind::OpenRouter] {
        let key = env_key(provider.secret());
        match catalog::stt_models(&client, provider, key.as_deref()).await {
            Ok(m) => println!("{provider:?}\n  wsadowo: {}\n  na żywo: {}", ids(&m.batch), ids(&m.realtime)),
            Err(e) => println!("{provider:?}: {e}"),
        }
    }

    let yours = catalog::openrouter_stt_id(SttProviderKind::ElevenLabs, "scribe_v2_realtime");
    println!("\nNowe modele STT (twój: {yours}):");
    match catalog::get_list(client.get(catalog::OPENROUTER_STT_URL)).await {
        Ok(models) => {
            for m in catalog::recent(&models, &yours, now, 8) {
                println!("  {} {} ({}){}", m.model.created.unwrap_or(0), m.model.name, m.model.id, if m.newer_than_yours { " — nowszy niż twój" } else { "" });
            }
        }
        Err(e) => println!("  {e}"),
    }

    let mut args = std::env::args().skip(1);
    let (Some(base), Some(model)) = (args.next(), args.next()) else { return };
    println!("\nNowe modele post-processingu ({base}, twój: {model}):");
    let mut rb = client.get(format!("{}/models", base.trim_end_matches('/')));
    if let Some(key) = env_key(SecretKey::LlmEndpoint) {
        rb = rb.bearer_auth(key);
    }
    match catalog::get_list(rb).await {
        Ok(models) => {
            let chat: Vec<_> = models.into_iter().filter(|m| m.chat).collect();
            let yours = if base.contains("openrouter.ai") { catalog::openrouter_base_id(&model) } else { &model };
            for m in catalog::recent(&chat, yours, now, 8) {
                println!("  {} {} ({}){}", m.model.created.unwrap_or(0), m.model.name, m.model.owner, if m.newer_than_yours { " — nowszy niż twój" } else { "" });
            }
        }
        Err(e) => println!("  {e}"),
    }
}

//! Model catalogs: the speech-to-text models of each provider for the pickers
//! in the settings window, and the models added recently for the "Nowe
//! modele" card of the overview page. ElevenLabs and xAI do not list their
//! STT models in an API (`/v1/models` has only their other models), so those
//! lists are built in from their docs (15.10.2026). OpenAI and OpenRouter
//! lists come from their APIs.

use std::time::Duration;

use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::stt::SttProviderKind;

/// OpenRouter's public catalog of transcription models (no key needed).
pub const OPENROUTER_STT_URL: &str = "https://openrouter.ai/api/v1/models?output_modalities=transcription";
/// OpenRouter's public catalog of text models.
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
const TIMEOUT: Duration = Duration::from_secs(10);
/// Models added within this many days count as new.
pub const NEW_DAYS: u64 = 45;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    /// Display name when the catalog has one, else the id.
    pub name: String,
    /// Unix seconds the catalog added the model.
    pub created: Option<u64>,
    /// Vendor: the part of the id before `/`, else `owned_by`.
    pub owner: String,
    /// List price, when the catalog or the provider's price list has one.
    pub price: Option<Price>,
    /// A chat request can use it as listed (see [`is_chat`]).
    #[serde(skip)]
    pub chat: bool,
}

/// Price in USD.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Price {
    /// Per hour of audio.
    Hour { usd: f64 },
    /// Per million input and output tokens.
    Tokens { input: f64, output: f64 },
}

impl CatalogModel {
    fn known(id: &str, usd_per_hour: Option<f64>) -> Self {
        Self {
            id: id.into(),
            name: id.into(),
            created: None,
            owner: guess_owner(id),
            price: usd_per_hour.map(|usd| Price::Hour { usd }),
            chat: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SttModels {
    pub batch: Vec<CatalogModel>,
    /// Empty when the provider has no streaming variant.
    pub realtime: Vec<CatalogModel>,
}

/// A recently added model for the overview.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NewModel {
    #[serde(flatten)]
    pub model: CatalogModel,
    /// Same vendor as the model in use and added after it.
    pub newer_than_yours: bool,
}

/// An OpenAI-style `{"data": [...]}` model list, newest first.
pub fn parse_list(body: &str) -> Result<Vec<CatalogModel>> {
    let v: Value = serde_json::from_str(body).map_err(|e| Error::InvalidResponse(e.to_string()))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidResponse(crate::tr!("missing data field", "brak pola data")))?;
    let mut models: Vec<CatalogModel> = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let text = |k: &str| m.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
            let owner = match id.split_once('/') {
                Some((vendor, _)) => vendor.to_string(),
                None => text("owned_by").map_or_else(|| guess_owner(&id), str::to_string),
            };
            let price = m.get("pricing").and_then(|p| parse_price(p, is_transcription(m)));
            Some(CatalogModel {
                name: text("name").unwrap_or(&id).to_string(),
                created: m.get("created").and_then(Value::as_u64).filter(|&t| t > 0),
                owner,
                chat: is_chat(m, &id, price),
                price,
                id,
            })
        })
        .collect();
    models.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

fn is_transcription(model: &Value) -> bool {
    model
        .pointer("/architecture/output_modalities")
        .and_then(Value::as_array)
        .is_some_and(|m| m.iter().any(|v| v == "transcription"))
}

/// OpenRouter lists image models, routers with a variable price (`-1`) or
/// none (`openrouter/free`), tools that bill no output (`scaledown/extract`),
/// variants (`:free`) and aliases (`~vendor/model-latest`) next to the chat
/// models. Lists without `architecture` (an OpenAI-compatible endpoint) are
/// all chat models.
fn is_chat(model: &Value, id: &str, price: Option<Price>) -> bool {
    let Some(output) = model.pointer("/architecture/output_modalities").and_then(Value::as_array) else {
        return true;
    };
    let bills_output = matches!(price, Some(Price::Tokens { output, .. }) if output > 0.0);
    output.iter().all(|v| v == "text") && bills_output && !id.contains(':') && !id.starts_with('~')
}

/// An OpenRouter model id without its variant (`vendor/model:nitro` →
/// `vendor/model`), as the catalog lists it.
pub fn openrouter_base_id(id: &str) -> &str {
    id.split_once(':').map_or(id, |(base, _)| base)
}

/// OpenRouter's `pricing`: USD per token as strings (`-1` = variable). Speech
/// models without an output price are billed per second of audio, except
/// Azure's MAI models, whose figure is per hour: `0.1` for MAI-Transcribe 2
/// matched a request's `usage.cost` (6 s → 0.000167 $) on 15.10.2026. No
/// per-second rate comes near 0.01 $ (that would be 36 $/h).
fn parse_price(pricing: &Value, transcription: bool) -> Option<Price> {
    let usd = |k: &str| pricing.get(k)?.as_str()?.parse::<f64>().ok().filter(|v| *v >= 0.0);
    let input = usd("prompt")?;
    let output = usd("completion").unwrap_or(0.0);
    if transcription && output == 0.0 {
        let usd = if input >= 0.01 { input } else { input * 3600.0 };
        return Some(Price::Hour { usd });
    }
    Some(Price::Tokens { input: input * 1e6, output: output * 1e6 })
}

/// Models the app knows without asking the provider, with USD per hour of
/// audio from the providers' price lists (15.10.2026; the gpt-4o models are
/// billed per token, these are OpenAI's per-minute estimates).
pub fn builtin_stt(provider: SttProviderKind) -> SttModels {
    let list = |models: &[(&str, Option<f64>)]| {
        models.iter().map(|&(id, usd)| CatalogModel::known(id, usd)).collect::<Vec<_>>()
    };
    match provider {
        SttProviderKind::ElevenLabs => SttModels {
            batch: list(&[("scribe_v2", Some(0.22)), ("scribe_v1", None)]),
            realtime: list(&[("scribe_v2_realtime", Some(0.39))]),
        },
        // One model for files and the WebSocket, streaming costs double.
        SttProviderKind::Xai => SttModels {
            batch: list(&[("grok-voice-transcribe-2.0", Some(0.10))]),
            realtime: list(&[("grok-voice-transcribe-2.0", Some(0.20))]),
        },
        SttProviderKind::OpenAi => SttModels {
            batch: list(&[
                ("gpt-transcribe", Some(0.27)),
                ("gpt-4o-transcribe", Some(0.36)),
                ("gpt-4o-mini-transcribe", Some(0.18)),
                ("whisper-1", Some(0.36)),
            ]),
            realtime: list(&[
                ("gpt-live-transcribe", Some(1.02)),
                ("gpt-realtime-whisper", Some(1.02)),
                ("gpt-4o-transcribe", Some(0.36)),
                ("gpt-4o-mini-transcribe", Some(0.18)),
            ]),
        },
        SttProviderKind::OpenRouter => {
            SttModels { batch: list(&[("openai/gpt-transcribe", Some(0.27))]), realtime: Vec::new() }
        }
    }
}

/// USD per hour of audio of `model` from the built-in list (the streaming
/// variant when the transcript came `live`), to estimate what a dictation cost.
pub fn stt_hourly_price(provider: SttProviderKind, model: &str, live: bool) -> Option<f64> {
    let models = builtin_stt(provider);
    let list = if live { &models.realtime } else { &models.batch };
    match list.iter().find(|m| m.id == model)?.price? {
        Price::Hour { usd } => Some(usd),
        Price::Tokens { .. } => None,
    }
}

fn is_openai_stt(id: &str) -> bool {
    id.contains("transcribe") || id.contains("whisper")
}

/// Fill in the prices `/v1/models` lacks from the built-in list.
fn with_prices(mut models: Vec<CatalogModel>, known: &[CatalogModel]) -> Vec<CatalogModel> {
    for m in &mut models {
        m.price = m.price.or_else(|| known.iter().find(|k| k.id == m.id).and_then(|k| k.price));
    }
    models
}

/// STT models of `provider`: from its API where there is one, else built in.
pub async fn stt_models(client: &Client, provider: SttProviderKind, key: Option<&str>) -> Result<SttModels> {
    match (provider, key) {
        (SttProviderKind::OpenRouter, _) => {
            let batch = get_list(client.get(OPENROUTER_STT_URL)).await?;
            Ok(SttModels { batch, realtime: Vec::new() })
        }
        (SttProviderKind::OpenAi, Some(key)) => {
            let models: Vec<_> = get_list(client.get(OPENAI_MODELS_URL).bearer_auth(key))
                .await?
                .into_iter()
                .filter(|m| is_openai_stt(&m.id))
                .collect();
            let known = builtin_stt(provider);
            Ok(SttModels {
                batch: with_prices(models.clone(), &known.batch),
                realtime: with_prices(models, &known.realtime),
            })
        }
        _ => Ok(builtin_stt(provider)),
    }
}

/// `GET {base}/models` of an OpenAI-compatible endpoint, with its key if any.
pub fn models_request(client: &Client, base_url: &str, key: Option<&str>) -> RequestBuilder {
    let request = client.get(format!("{}/models", base_url.trim().trim_end_matches('/')));
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

/// GET an OpenAI-style model list.
pub async fn get_list(request: RequestBuilder) -> Result<Vec<CatalogModel>> {
    let timed_out = |e: reqwest::Error| if e.is_timeout() { Error::Timeout(TIMEOUT) } else { e.into() };
    let resp = request.timeout(TIMEOUT).send().await.map_err(timed_out)?;
    let status = resp.status();
    let body = resp.text().await.map_err(timed_out)?;
    if !status.is_success() {
        return Err(Error::from_status(status.as_u16(), &body));
    }
    parse_list(&body)
}

/// Where a model a provider calls `id` sits in OpenRouter's STT catalog
/// (`scribe_v2_realtime` → `elevenlabs/scribe-v2`), to compare dates.
pub fn openrouter_stt_id(provider: SttProviderKind, id: &str) -> String {
    let vendor = match provider {
        SttProviderKind::OpenRouter => return id.to_string(),
        SttProviderKind::ElevenLabs => "elevenlabs",
        SttProviderKind::Xai => "x-ai",
        SttProviderKind::OpenAi => "openai",
    };
    let id = id.to_lowercase().replace('_', "-");
    format!("{vendor}/{}", id.strip_suffix("-realtime").unwrap_or(&id))
}

/// Vendor of a model id without a catalog entry.
fn guess_owner(id: &str) -> String {
    if let Some((vendor, _)) = id.split_once('/') {
        return vendor.to_string();
    }
    let id = id.to_lowercase();
    let vendor = [
        ("claude", "anthropic"),
        ("gpt", "openai"),
        ("whisper", "openai"),
        ("gemini", "google"),
        ("grok", "x-ai"),
        ("scribe", "elevenlabs"),
    ]
    .iter()
    .find(|(prefix, _)| id.starts_with(prefix))
    .map_or("", |(_, vendor)| vendor);
    vendor.to_string()
}

/// Models added in the last [`NEW_DAYS`], newest first, at most `limit`,
/// without `yours` (the model in use, as the catalog names it).
pub fn recent(models: &[CatalogModel], yours: &str, now_unix: u64, limit: usize) -> Vec<NewModel> {
    let mine = models.iter().find(|m| m.id == yours);
    let owner = mine.map_or_else(|| guess_owner(yours), |m| m.owner.clone());
    let since = mine.and_then(|m| m.created);
    let cutoff = now_unix.saturating_sub(NEW_DAYS * 86_400);
    models
        .iter()
        .filter(|m| m.id != yours && m.created.is_some_and(|t| t >= cutoff))
        .take(limit)
        .map(|m| NewModel {
            model: m.clone(),
            newer_than_yours: !owner.is_empty() && m.owner == owner && since.is_none_or(|s| m.created > Some(s)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_000_000;
    const DAY: u64 = 86_400;

    fn catalog() -> Vec<CatalogModel> {
        let body = serde_json::json!({"data": [
            {"id": "elevenlabs/scribe-v2", "name": "ElevenLabs: Scribe v2", "created": NOW - 8 * DAY},
            {"id": "google/gemini-3.5-transcribe", "name": "Google: Gemini 3.5 Transcribe", "created": NOW - 20 * DAY},
            {"id": "elevenlabs/scribe-v3", "name": "ElevenLabs: Scribe v3", "created": NOW - DAY},
            {"id": "openai/whisper-1", "name": "OpenAI: Whisper 1", "created": NOW - 170 * DAY},
            {"id": "claude-haiku-5-5", "owned_by": "anthropic", "created": NOW - 17 * DAY},
            {"id": "elevenlabs/scribe-v2", "created": NOW - 8 * DAY},
        ]});
        parse_list(&body.to_string()).unwrap()
    }

    #[test]
    fn parses_newest_first_with_vendor() {
        let models = catalog();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["elevenlabs/scribe-v3", "elevenlabs/scribe-v2", "claude-haiku-5-5", "google/gemini-3.5-transcribe", "openai/whisper-1"]
        );
        assert_eq!((models[0].owner.as_str(), models[0].name.as_str()), ("elevenlabs", "ElevenLabs: Scribe v3"));
        assert_eq!((models[2].owner.as_str(), models[2].name.as_str()), ("anthropic", "claude-haiku-5-5"));
        assert!(parse_list("{}").is_err());
    }

    #[test]
    fn recent_marks_newer_models_of_your_vendor() {
        let models = catalog();
        let yours = openrouter_stt_id(SttProviderKind::ElevenLabs, "scribe_v2_realtime");
        assert_eq!(yours, "elevenlabs/scribe-v2");
        let news = recent(&models, &yours, NOW, 10);
        let flags: Vec<_> = news.iter().map(|n| (n.model.id.as_str(), n.newer_than_yours)).collect();
        // Whisper is too old to be news; your own model is not listed.
        assert_eq!(
            flags,
            [("elevenlabs/scribe-v3", true), ("claude-haiku-5-5", false), ("google/gemini-3.5-transcribe", false)]
        );

        // A model the catalog does not have: every new model of its vendor is newer.
        let news = recent(&models, "claude-opus-9", NOW, 2);
        let flags: Vec<_> = news.iter().map(|n| (n.model.id.as_str(), n.newer_than_yours)).collect();
        assert_eq!(flags, [("elevenlabs/scribe-v3", false), ("elevenlabs/scribe-v2", false)]);
        let news = recent(&models, "claude-opus-9", NOW, 5);
        assert!(news.iter().any(|n| n.model.id == "claude-haiku-5-5" && n.newer_than_yours));
    }

    #[test]
    fn builtin_lists_contain_the_defaults() {
        let defaults = crate::config::SttModels::default();
        let has = |list: &[CatalogModel], id: &str| list.iter().any(|m| m.id == id);
        let el = builtin_stt(SttProviderKind::ElevenLabs);
        assert!(has(&el.batch, &defaults.elevenlabs) && has(&el.realtime, &defaults.elevenlabs_realtime));
        let oa = builtin_stt(SttProviderKind::OpenAi);
        assert!(has(&oa.batch, &defaults.openai) && has(&oa.realtime, &defaults.openai_realtime));
        assert!(has(&builtin_stt(SttProviderKind::Xai).batch, &defaults.xai));
        assert!(has(&builtin_stt(SttProviderKind::OpenRouter).batch, &defaults.openrouter));
        assert!(is_openai_stt("gpt-4o-mini-transcribe") && !is_openai_stt("gpt-6-sol"));
    }

    fn price_of(model: Value) -> Option<Price> {
        parse_list(&serde_json::json!({ "data": [model] }).to_string()).unwrap()[0].price
    }

    #[test]
    fn prices_per_hour_of_audio_or_per_million_tokens() {
        let stt = |id: &str, prompt: &str, completion: &str| {
            price_of(serde_json::json!({
                "id": id,
                "architecture": {"output_modalities": ["transcription"]},
                "pricing": {"prompt": prompt, "completion": completion},
            }))
        };
        let hourly = |p: Option<Price>| match p {
            Some(Price::Hour { usd }) => (usd * 1000.0).round() / 1000.0,
            other => panic!("{other:?}"),
        };
        // Per second of audio (catalog of 15.10.2026).
        assert_eq!(hourly(stt("x-ai/grok-stt-1.0", "0.0000277777777778", "0")), 0.1);
        assert_eq!(hourly(stt("openai/gpt-transcribe", "0.000075", "0")), 0.27);
        // Azure's MAI models: per hour.
        assert_eq!(hourly(stt("microsoft/mai-transcribe-2", "0.1", "0")), 0.1);
        // Speech models billed by tokens, and text models.
        assert_eq!(stt("google/gemini-3.5-transcribe", "0.000002", "0.000012"), Some(Price::Tokens { input: 2.0, output: 12.0 }));
        let text = |pricing: Value| price_of(serde_json::json!({"id": "a/b", "pricing": pricing}));
        assert_eq!(text(serde_json::json!({"prompt": "0.000001", "completion": "0.000005"})), Some(Price::Tokens { input: 1.0, output: 5.0 }));
        assert_eq!(text(serde_json::json!({"prompt": "-1", "completion": "-1"})), None);
        assert_eq!(price_of(serde_json::json!({"id": "claude-haiku-5-5"})), None);
    }

    #[test]
    fn api_lists_get_built_in_prices() {
        let api = vec![CatalogModel::known("gpt-transcribe", None), CatalogModel::known("gpt-9-transcribe", None)];
        let priced = with_prices(api, &builtin_stt(SttProviderKind::OpenAi).batch);
        assert_eq!(priced[0].price, Some(Price::Hour { usd: 0.27 }));
        assert_eq!(priced[1].price, None);
        let xai = builtin_stt(SttProviderKind::Xai);
        assert_ne!(xai.batch[0].price, xai.realtime[0].price);
    }

    #[test]
    fn chat_models_skip_images_routers_tools_variants_and_aliases() {
        let text = |id: &str, out: &[&str], prompt: &str, completion: &str| {
            serde_json::json!({
                "id": id, "created": NOW - DAY,
                "architecture": {"output_modalities": out},
                "pricing": {"prompt": prompt, "completion": completion},
            })
        };
        let body = serde_json::json!({"data": [
            text("deepseek/deepseek-v4.1-flash", &["text"], "0.0000003", "0.0000012"),
            text("deepseek/deepseek-v4.1-flash:free", &["text"], "0", "0"),
            text("~deepseek/deepseek-flash-latest", &["text"], "0.0000003", "0.0000012"),
            text("google/gemini-4-image", &["image", "text"], "0.000001", "0.000004"),
            text("openrouter/auto", &["text"], "-1", "-1"),
            text("openrouter/free", &["text"], "0", "0"),
            text("scaledown/extract", &["text"], "0.00000004", "0"),
            {"id": "claude-haiku-5-5", "created": NOW - DAY},
        ]});
        let chat: Vec<_> = parse_list(&body.to_string()).unwrap().into_iter().filter(|m| m.chat).map(|m| m.id).collect();
        assert_eq!(chat, ["claude-haiku-5-5", "deepseek/deepseek-v4.1-flash"]);
        assert_eq!(openrouter_base_id("deepseek/deepseek-v4.1-flash:nitro"), "deepseek/deepseek-v4.1-flash");
        assert_eq!(openrouter_base_id("x-ai/grok-5"), "x-ai/grok-5");
    }

    #[test]
    fn hourly_price_follows_streaming() {
        assert_eq!(stt_hourly_price(SttProviderKind::ElevenLabs, "scribe_v2_realtime", true), Some(0.39));
        assert_eq!(stt_hourly_price(SttProviderKind::ElevenLabs, "scribe_v2", false), Some(0.22));
        assert_eq!(stt_hourly_price(SttProviderKind::Xai, "grok-voice-transcribe-2.0", true), Some(0.20));
        assert_eq!(stt_hourly_price(SttProviderKind::ElevenLabs, "scribe_v1", false), None);
        assert_eq!(stt_hourly_price(SttProviderKind::OpenRouter, "microsoft/mai-transcribe-2", false), None);
    }
}

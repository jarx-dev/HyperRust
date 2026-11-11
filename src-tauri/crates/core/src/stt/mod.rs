//! Speech-to-text providers.
//!
//! Every provider is split into pure request building / response parsing
//! (unit-tested on recorded fixtures) and a thin executor. Adding a provider
//! means implementing [`BatchProvider`] and, if it can stream, [`LiveProtocol`],
//! then registering it in [`batch_provider`] / [`live_protocol`].

pub mod elevenlabs;
pub mod live;
pub mod openai;
pub mod openrouter;
pub mod xai;

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::{SecretKey, SttSettings};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SttProviderKind {
    #[default]
    #[serde(rename = "elevenlabs")]
    ElevenLabs,
    Xai,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

impl SttProviderKind {
    pub const ALL: [SttProviderKind; 4] = [
        SttProviderKind::ElevenLabs,
        SttProviderKind::Xai,
        SttProviderKind::OpenAi,
        SttProviderKind::OpenRouter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SttProviderKind::ElevenLabs => "ElevenLabs",
            SttProviderKind::Xai => "xAI",
            SttProviderKind::OpenAi => "OpenAI",
            SttProviderKind::OpenRouter => "OpenRouter",
        }
    }

    pub fn secret(self) -> SecretKey {
        match self {
            SttProviderKind::ElevenLabs => SecretKey::ElevenLabs,
            SttProviderKind::Xai => SecretKey::Xai,
            SttProviderKind::OpenAi => SecretKey::OpenAi,
            SttProviderKind::OpenRouter => SecretKey::OpenRouter,
        }
    }

    /// Keys a dictation may use, the main one first: ElevenLabs also takes a
    /// second account's key for when the first account fails.
    pub fn secrets(self) -> Vec<SecretKey> {
        match self {
            SttProviderKind::ElevenLabs => vec![SecretKey::ElevenLabs, SecretKey::ElevenLabsBackup],
            other => vec![other.secret()],
        }
    }

    pub fn supports_streaming(self) -> bool {
        live_protocol_supported(self)
    }

    /// Host touched by connection warm-up.
    pub fn base_url(self) -> &'static str {
        match self {
            SttProviderKind::ElevenLabs => "https://api.elevenlabs.io",
            SttProviderKind::Xai => "https://api.x.ai",
            SttProviderKind::OpenAi => "https://api.openai.com",
            SttProviderKind::OpenRouter => "https://openrouter.ai",
        }
    }
}

/// Everything a provider needs to transcribe one recording.
#[derive(Debug, Clone)]
pub struct SttRequest<'a> {
    /// Mono PCM16 at [`crate::audio::TARGET_RATE`].
    pub pcm: &'a [i16],
    pub model: &'a str,
    pub language: Option<&'a str>,
    /// Words to bias recognition towards (ElevenLabs keyterms); other providers ignore them.
    pub keyterms: &'a [String],
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartValue {
    Text(String),
    File { filename: String, mime: String, bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: String,
    pub value: PartValue,
}

impl Part {
    pub fn text(name: &str, value: impl Into<String>) -> Self {
        Part { name: name.into(), value: PartValue::Text(value.into()) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Json(serde_json::Value),
    /// Parts in the exact order they must be sent (xAI requires `file` last).
    Multipart(Vec<Part>),
}

/// A provider request described as data, so tests can inspect it.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpSpec {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl HttpSpec {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn part(&self, name: &str) -> Option<&PartValue> {
        match &self.body {
            Body::Multipart(parts) => parts.iter().find(|p| p.name == name).map(|p| &p.value),
            Body::Json(_) => None,
        }
    }
}

pub trait BatchProvider: Send + Sync {
    fn build(&self, req: &SttRequest<'_>, api_key: &str) -> HttpSpec;
    /// Parse a 2xx body into the transcript text.
    fn parse(&self, body: &[u8]) -> Result<String>;
    /// USD the provider billed for the request, when the response says.
    fn cost(&self, _body: &[u8]) -> Option<f64> {
        None
    }
}

pub fn batch_provider(kind: SttProviderKind) -> Box<dyn BatchProvider> {
    match kind {
        SttProviderKind::ElevenLabs => Box::new(elevenlabs::ElevenLabsBatch),
        SttProviderKind::Xai => Box::new(xai::XaiBatch),
        SttProviderKind::OpenAi => Box::new(openai::OpenAiBatch),
        SttProviderKind::OpenRouter => Box::new(openrouter::OpenRouterBatch),
    }
}

fn live_protocol_supported(kind: SttProviderKind) -> bool {
    !matches!(kind, SttProviderKind::OpenRouter)
}

/// Streaming protocol for providers that accept audio while the user speaks.
pub fn live_protocol(settings: &SttSettings, api_key: &str) -> Option<Box<dyn live::LiveProtocol>> {
    let model = settings.realtime_model().to_string();
    let lang = settings.language().map(str::to_string);
    match settings.provider {
        SttProviderKind::ElevenLabs => Some(Box::new(elevenlabs::ElevenLabsLive::new(
            api_key,
            &model,
            lang.as_deref(),
            crate::dictionary::keyterms(&settings.dictionary),
        ))),
        SttProviderKind::Xai => Some(Box::new(xai::XaiLive::new(api_key, &model, lang.as_deref()))),
        SttProviderKind::OpenAi => Some(Box::new(openai::OpenAiLive::new(api_key, &model, lang.as_deref()))),
        SttProviderKind::OpenRouter => None,
    }
}

/// Send a batch request: the transcript and, when the provider reports it,
/// what the request cost.
pub async fn transcribe_batch(
    client: &Client,
    provider: &dyn BatchProvider,
    req: &SttRequest<'_>,
    api_key: &str,
    timeout: Duration,
) -> Result<(String, Option<f64>)> {
    let spec = provider.build(req, api_key);
    let (status, body) = execute(client, spec, timeout).await?;
    if !(200..300).contains(&status) {
        return Err(Error::from_status(status, &String::from_utf8_lossy(&body)));
    }
    Ok((provider.parse(&body)?, provider.cost(&body)))
}

pub async fn execute(client: &Client, spec: HttpSpec, timeout: Duration) -> Result<(u16, Vec<u8>)> {
    let mut rb = client.post(&spec.url).timeout(timeout);
    for (k, v) in &spec.headers {
        rb = rb.header(k, v);
    }
    rb = match spec.body {
        Body::Json(v) => rb.json(&v),
        Body::Multipart(parts) => {
            let mut form = reqwest::multipart::Form::new();
            for p in parts {
                form = match p.value {
                    PartValue::Text(t) => form.text(p.name, t),
                    PartValue::File { filename, mime, bytes } => form.part(
                        p.name,
                        reqwest::multipart::Part::bytes(bytes)
                            .file_name(filename)
                            .mime_str(&mime)
                            .map_err(|e| Error::Other(e.to_string()))?,
                    ),
                };
            }
            rb.multipart(form)
        }
    };
    let resp = rb.send().await.map_err(|e| {
        if e.is_timeout() {
            Error::Timeout(timeout)
        } else {
            Error::Network(e.to_string())
        }
    })?;
    let status = resp.status().as_u16();
    let body = resp.bytes().await.map_err(Error::from)?.to_vec();
    Ok((status, body))
}

/// Read `text` from a JSON body (shared by every batch API).
pub(crate) fn text_field(body: &[u8], field: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| Error::InvalidResponse(format!("JSON: {e}")))?;
    v.get(field)
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .ok_or_else(|| Error::InvalidResponse(crate::tr!("missing field `{field}`", "brak pola `{field}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kinds_serialize_to_stable_ids() {
        let ids: Vec<String> = SttProviderKind::ALL
            .iter()
            .map(|k| serde_json::to_string(k).unwrap())
            .collect();
        assert_eq!(ids, ["\"elevenlabs\"", "\"xai\"", "\"openai\"", "\"openrouter\""]);
    }

    #[test]
    fn streaming_support() {
        assert!(SttProviderKind::ElevenLabs.supports_streaming());
        assert!(SttProviderKind::Xai.supports_streaming());
        assert!(SttProviderKind::OpenAi.supports_streaming());
        assert!(!SttProviderKind::OpenRouter.supports_streaming());
    }

    #[test]
    fn text_field_errors_are_typed() {
        assert_eq!(text_field(br#"{"text":" ok "}"#, "text").unwrap(), "ok");
        assert!(matches!(text_field(b"{}", "text"), Err(Error::InvalidResponse(_))));
        assert!(matches!(text_field(b"<html>", "text"), Err(Error::InvalidResponse(_))));
    }
}

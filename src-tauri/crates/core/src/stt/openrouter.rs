//! OpenRouter speech-to-text: `POST /api/v1/audio/transcriptions` with base64
//! audio in JSON. One key and one bill for every vendor OpenRouter routes to;
//! there is no streaming variant.

use base64::Engine;
use serde_json::json;

use super::{text_field, BatchProvider, Body, HttpSpec, SttRequest};
use crate::audio::{wav_bytes, TARGET_RATE};
use crate::error::Result;

pub const URL: &str = "https://openrouter.ai/api/v1/audio/transcriptions";

pub struct OpenRouterBatch;

impl BatchProvider for OpenRouterBatch {
    fn build(&self, req: &SttRequest<'_>, api_key: &str) -> HttpSpec {
        // WAV rather than headerless `pcm`: every routed vendor accepts it.
        let audio = base64::engine::general_purpose::STANDARD.encode(wav_bytes(req.pcm, TARGET_RATE));
        let mut body = json!({
            "model": req.model,
            "input_audio": { "data": audio, "format": "wav" },
        });
        if let Some(lang) = req.language {
            body["language"] = json!(lang);
        }
        HttpSpec {
            url: URL.into(),
            headers: vec![
                ("Authorization".into(), format!("Bearer {api_key}")),
                ("X-Title".into(), "HyperRust".into()),
            ],
            body: Body::Json(body),
        }
    }

    fn parse(&self, body: &[u8]) -> Result<String> {
        text_field(body, "text")
    }

    /// `usage.cost`: what this request was billed.
    fn cost(&self, body: &[u8]) -> Option<f64> {
        serde_json::from_slice::<serde_json::Value>(body).ok()?.pointer("/usage/cost")?.as_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_json_with_base64_wav() {
        let spec = OpenRouterBatch.build(
            &SttRequest { pcm: &[0; 8], model: "openai/gpt-transcribe", language: Some("pl"), keyterms: &[] },
            "K",
        );
        assert_eq!(spec.url, URL);
        assert_eq!(spec.header("Authorization"), Some("Bearer K"));
        let Body::Json(v) = &spec.body else { panic!() };
        assert_eq!(v["model"], "openai/gpt-transcribe");
        assert_eq!(v["language"], "pl");
        assert_eq!(v["input_audio"]["format"], "wav");
        let wav = base64::engine::general_purpose::STANDARD
            .decode(v["input_audio"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 16);
    }

    #[test]
    fn parses_recorded_response() {
        let body = include_bytes!("../../tests/fixtures/openrouter_stt.json");
        assert!(OpenRouterBatch.parse(body).unwrap().contains("popraw w pliku package.json"));
        assert_eq!(OpenRouterBatch.cost(body), Some(0.000975));
        assert_eq!(OpenRouterBatch.cost(br#"{"text":"x"}"#), None);
    }
}

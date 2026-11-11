//! OpenAI transcription: batch `POST /v1/audio/transcriptions` and a Realtime
//! transcription-only session (`gpt-live-transcribe`, 24 kHz PCM, manual commit).

use serde_json::{json, Value};

use super::live::{pcm_base64, LiveEvent, LiveProtocol, WsOut};
use super::{text_field, BatchProvider, HttpSpec, Part, PartValue, SttRequest};
use crate::audio::{f32_to_i16, i16_to_f32, wav_bytes, Resampler, TARGET_RATE};
use crate::error::{Error, Result};

pub const BATCH_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
pub const REALTIME_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
/// The Realtime API accepts `audio/pcm` only at 24 kHz.
pub const REALTIME_RATE: u32 = 24_000;

/// `gpt-transcribe` / `gpt-live-transcribe` take `languages[]`; older models take `language`.
fn uses_language_list(model: &str) -> bool {
    model.starts_with("gpt-transcribe") || model.starts_with("gpt-live-transcribe")
}

pub struct OpenAiBatch;

impl BatchProvider for OpenAiBatch {
    fn build(&self, req: &SttRequest<'_>, api_key: &str) -> HttpSpec {
        let mut parts = vec![Part::text("model", req.model)];
        if let Some(lang) = req.language {
            if uses_language_list(req.model) {
                parts.push(Part::text("languages[]", lang));
            } else {
                parts.push(Part::text("language", lang));
            }
        }
        parts.push(Part::text("response_format", "json"));
        // Raw PCM is rejected: the file needs a container and an extension.
        parts.push(Part {
            name: "file".into(),
            value: PartValue::File {
                filename: "audio.wav".into(),
                mime: "audio/wav".into(),
                bytes: wav_bytes(req.pcm, TARGET_RATE),
            },
        });
        HttpSpec {
            url: BATCH_URL.into(),
            headers: vec![("Authorization".into(), format!("Bearer {api_key}"))],
            body: super::Body::Multipart(parts),
        }
    }

    fn parse(&self, body: &[u8]) -> Result<String> {
        text_field(body, "text")
    }
}

pub struct OpenAiLive {
    api_key: String,
    model: String,
    language: Option<String>,
    resampler: Resampler,
    finished: bool,
    /// Item ids in commit order with their completed transcripts.
    items: Vec<(String, Option<String>)>,
}

impl OpenAiLive {
    pub fn new(api_key: &str, model: &str, language: Option<&str>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            language: language.map(str::to_string),
            resampler: Resampler::new(TARGET_RATE, REALTIME_RATE),
            finished: false,
            items: Vec::new(),
        }
    }

    fn session_update(&self) -> Value {
        let mut transcription = json!({ "model": self.model });
        if let Some(lang) = &self.language {
            if uses_language_list(&self.model) {
                transcription["languages"] = json!([lang]);
            } else {
                transcription["language"] = json!(lang);
            }
        }
        json!({
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": {
                    "input": {
                        "format": { "type": "audio/pcm", "rate": REALTIME_RATE },
                        "transcription": transcription,
                        // Push-to-talk: the key release is the commit.
                        "turn_detection": null
                    }
                }
            }
        })
    }

    fn all_completed(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|(_, t)| t.is_some())
    }
}

impl LiveProtocol for OpenAiLive {
    fn name(&self) -> &'static str {
        "OpenAI"
    }

    fn url(&self) -> String {
        REALTIME_URL.into()
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![("Authorization".into(), format!("Bearer {}", self.api_key))]
    }

    fn start_frames(&mut self) -> Vec<WsOut> {
        vec![WsOut::Text(self.session_update().to_string())]
    }

    fn audio_frame(&mut self, pcm: &[i16]) -> WsOut {
        let up = f32_to_i16(&self.resampler.process(&i16_to_f32(pcm)));
        WsOut::Text(json!({ "type": "input_audio_buffer.append", "audio": pcm_base64(&up) }).to_string())
    }

    fn finish_frames(&mut self) -> Vec<WsOut> {
        self.finished = true;
        vec![WsOut::Text(r#"{"type":"input_audio_buffer.commit"}"#.into())]
    }

    fn handle(&mut self, text: &str) -> LiveEvent {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return LiveEvent::Ignore;
        };
        let item_id = v.get("item_id").and_then(Value::as_str).unwrap_or("").to_string();
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "session.created" | "session.updated" => LiveEvent::Started,
            "input_audio_buffer.committed" => {
                if !self.items.iter().any(|(id, _)| *id == item_id) {
                    self.items.push((item_id, None));
                }
                LiveEvent::Ignore
            }
            "conversation.item.input_audio_transcription.delta" => {
                LiveEvent::Partial(v.get("delta").and_then(Value::as_str).unwrap_or("").to_string())
            }
            "conversation.item.input_audio_transcription.completed" => {
                let transcript = v.get("transcript").and_then(Value::as_str).unwrap_or("").to_string();
                match self.items.iter_mut().find(|(id, _)| *id == item_id) {
                    Some(slot) => slot.1 = Some(transcript),
                    None => self.items.push((item_id, Some(transcript))),
                }
                if self.finished && self.all_completed() {
                    LiveEvent::Done
                } else {
                    LiveEvent::Final
                }
            }
            "conversation.item.input_audio_transcription.failed" => LiveEvent::Error(Error::Provider(
                v.pointer("/error/message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| crate::tr!("transcription failed", "transkrypcja nieudana")),
            )),
            "error" => {
                let code = v.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
                let message = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("").to_string();
                LiveEvent::Error(match code {
                    "input_audio_buffer_commit_empty" => Error::NoSpeech,
                    "invalid_api_key" => Error::Unauthorized { status: 401, message },
                    "rate_limit_exceeded" => Error::RateLimited { status: 429, message },
                    "insufficient_quota" => Error::QuotaExceeded { status: 429, message },
                    _ => Error::Provider(format!("{code}: {message}")),
                })
            }
            _ => LiveEvent::Ignore,
        }
    }

    fn transcript(&self) -> String {
        let mut out = String::new();
        for (_, t) in &self.items {
            if let Some(t) = t {
                super::live::append_segment(&mut out, t);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::Body;

    #[test]
    fn batch_wraps_wav_and_uses_language_list_for_new_models() {
        let spec = OpenAiBatch.build(
            &SttRequest { pcm: &[0; 160], model: "gpt-transcribe", language: Some("pl"), keyterms: &[] },
            "K",
        );
        assert_eq!(spec.header("Authorization"), Some("Bearer K"));
        assert_eq!(spec.part("languages[]"), Some(&PartValue::Text("pl".into())));
        assert_eq!(spec.part("language"), None);
        match spec.part("file") {
            Some(PartValue::File { filename, mime, bytes }) => {
                assert_eq!((filename.as_str(), mime.as_str()), ("audio.wav", "audio/wav"));
                assert_eq!(&bytes[..4], b"RIFF");
                assert_eq!(bytes.len(), 44 + 320);
            }
            other => panic!("{other:?}"),
        }
        let legacy = OpenAiBatch.build(&SttRequest { pcm: &[], model: "whisper-1", language: Some("pl"), keyterms: &[] }, "K");
        assert_eq!(legacy.part("language"), Some(&PartValue::Text("pl".into())));
        let Body::Multipart(parts) = legacy.body else { panic!() };
        assert_eq!(parts.last().unwrap().name, "file");
    }

    #[test]
    fn parses_documented_batch_response() {
        let body = br#"{"text":"Dodaj testy.","languages":[{"code":"pl"}],"usage":{"type":"duration","seconds":2}}"#;
        assert_eq!(OpenAiBatch.parse(body).unwrap(), "Dodaj testy.");
    }

    #[test]
    fn session_update_is_transcription_only_without_vad() {
        let mut p = OpenAiLive::new("K", "gpt-live-transcribe", Some("pl"));
        let WsOut::Text(t) = &p.start_frames()[0] else { panic!() };
        let v: Value = serde_json::from_str(t).unwrap();
        assert_eq!(v["session"]["type"], "transcription");
        let input = &v["session"]["audio"]["input"];
        assert_eq!(input["format"]["rate"], 24000);
        assert!(input["turn_detection"].is_null());
        assert_eq!(input["transcription"]["languages"][0], "pl");
        assert_eq!(input["transcription"]["model"], "gpt-live-transcribe");
    }

    #[test]
    fn audio_is_upsampled_to_24k() {
        let mut p = OpenAiLive::new("K", "gpt-live-transcribe", None);
        let WsOut::Text(t) = p.audio_frame(&[1000; 1600]) else { panic!() };
        let v: Value = serde_json::from_str(&t).unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(v["audio"].as_str().unwrap())
            .unwrap();
        let samples = bytes.len() / 2;
        assert!((2390..=2400).contains(&samples), "{samples}");
    }

    #[test]
    fn completes_on_the_committed_item() {
        let mut p = OpenAiLive::new("K", "gpt-live-transcribe", None);
        assert_eq!(p.handle(r#"{"type":"session.updated"}"#), LiveEvent::Started);
        assert_eq!(
            p.handle(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"i1","delta":"Na"}"#),
            LiveEvent::Partial("Na".into())
        );
        p.finish_frames();
        p.handle(r#"{"type":"input_audio_buffer.committed","item_id":"i1"}"#);
        assert_eq!(
            p.handle(r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"i1","transcript":"Napraw build."}"#),
            LiveEvent::Done
        );
        assert_eq!(p.transcript(), "Napraw build.");
    }

    #[test]
    fn empty_commit_means_no_speech() {
        let mut p = OpenAiLive::new("K", "m", None);
        assert!(matches!(
            p.handle(r#"{"type":"error","error":{"code":"input_audio_buffer_commit_empty","message":"x"}}"#),
            LiveEvent::Error(Error::NoSpeech)
        ));
    }
}

//! ElevenLabs Scribe: batch `POST /v1/speech-to-text` and the realtime
//! WebSocket `/v1/speech-to-text/realtime` (manual commit on key release).

use serde_json::{json, Value};

use super::live::{append_segment, pcm_base64, LiveEvent, LiveProtocol, WsOut};
use super::{text_field, BatchProvider, HttpSpec, Part, PartValue, SttRequest};
use crate::audio::{pcm_to_le_bytes, TARGET_RATE};
use crate::error::{Error, Result};

pub const BATCH_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";
pub const REALTIME_URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";

pub struct ElevenLabsBatch;

impl BatchProvider for ElevenLabsBatch {
    fn build(&self, req: &SttRequest<'_>, api_key: &str) -> HttpSpec {
        let mut parts = vec![
            Part::text("model_id", req.model),
            // Raw 16 kHz PCM16 is accepted directly and is the lowest-latency input.
            Part::text("file_format", "pcm_s16le_16"),
            Part::text("tag_audio_events", "false"),
            Part::text("timestamps_granularity", "none"),
        ];
        if let Some(lang) = req.language {
            parts.push(Part::text("language_code", lang));
        }
        parts.extend(req.keyterms.iter().map(|t| Part::text("keyterms", t.as_str())));
        parts.push(Part {
            name: "file".into(),
            value: PartValue::File {
                filename: "audio.pcm".into(),
                mime: "application/octet-stream".into(),
                bytes: pcm_to_le_bytes(req.pcm),
            },
        });
        HttpSpec {
            url: BATCH_URL.into(),
            headers: vec![("xi-api-key".into(), api_key.into())],
            body: super::Body::Multipart(parts),
        }
    }

    fn parse(&self, body: &[u8]) -> Result<String> {
        text_field(body, "text")
    }
}

pub struct ElevenLabsLive {
    api_key: String,
    model: String,
    language: Option<String>,
    keyterms: Vec<String>,
    committed: String,
    finished: bool,
}

impl ElevenLabsLive {
    pub fn new(api_key: &str, model: &str, language: Option<&str>, keyterms: Vec<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            language: language.map(str::to_string),
            keyterms,
            committed: String::new(),
            finished: false,
        }
    }

    fn chunk(audio_base_64: &str, commit: bool) -> WsOut {
        WsOut::Text(
            json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": audio_base_64,
                "commit": commit,
                "sample_rate": TARGET_RATE,
            })
            .to_string(),
        )
    }
}

impl LiveProtocol for ElevenLabsLive {
    fn name(&self) -> &'static str {
        "ElevenLabs"
    }

    fn url(&self) -> String {
        let mut url = url::Url::parse(REALTIME_URL).expect("static URL");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("model_id", &self.model);
            q.append_pair("audio_format", "pcm_16000");
            // Manual commit: the key release is the only end of utterance.
            q.append_pair("commit_strategy", "manual");
            if let Some(lang) = &self.language {
                q.append_pair("language_code", lang);
            }
            for t in &self.keyterms {
                q.append_pair("keyterms", t);
            }
        }
        url.to_string()
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![("xi-api-key".into(), self.api_key.clone())]
    }

    fn audio_frame(&mut self, pcm: &[i16]) -> WsOut {
        Self::chunk(&pcm_base64(pcm), false)
    }

    fn finish_frames(&mut self) -> Vec<WsOut> {
        self.finished = true;
        vec![Self::chunk("", true)]
    }

    fn handle(&mut self, text: &str) -> LiveEvent {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return LiveEvent::Ignore;
        };
        let kind = v.get("message_type").and_then(Value::as_str).unwrap_or("");
        let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match kind {
            "session_started" => LiveEvent::Started,
            "partial_transcript" => LiveEvent::Partial(field("text")),
            "committed_transcript" => {
                append_segment(&mut self.committed, &field("text"));
                if self.finished {
                    LiveEvent::Done
                } else {
                    LiveEvent::Final
                }
            }
            "auth_error" => LiveEvent::Error(Error::Unauthorized { status: 401, message: field("error") }),
            "quota_exceeded" => LiveEvent::Error(Error::QuotaExceeded { status: 401, message: field("error") }),
            "rate_limited" | "commit_throttled" | "resource_exhausted" | "queue_overflow" => {
                LiveEvent::Error(Error::RateLimited { status: 429, message: field("error") })
            }
            "insufficient_audio_activity" => LiveEvent::Error(Error::NoSpeech),
            k if k == "error" || k.ends_with("_error") || k.ends_with("_exceeded") || k == "invalid_request" => {
                LiveEvent::Error(Error::Provider(format!("{k}: {}", field("error"))))
            }
            _ => LiveEvent::Ignore,
        }
    }

    fn transcript(&self) -> String {
        self.committed.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::Body;

    #[test]
    fn batch_request_sends_raw_pcm_with_language() {
        let pcm = [1i16, 2, 3];
        let keyterms = ["TEKSOFT".to_string(), "Claude Code".to_string()];
        let spec = ElevenLabsBatch.build(
            &SttRequest { pcm: &pcm, model: "scribe_v2", language: Some("pl"), keyterms: &keyterms },
            "KEY",
        );
        assert_eq!(spec.url, BATCH_URL);
        assert_eq!(spec.header("xi-api-key"), Some("KEY"));
        assert_eq!(spec.part("model_id"), Some(&PartValue::Text("scribe_v2".into())));
        assert_eq!(spec.part("file_format"), Some(&PartValue::Text("pcm_s16le_16".into())));
        assert_eq!(spec.part("language_code"), Some(&PartValue::Text("pl".into())));
        match spec.part("file") {
            Some(PartValue::File { bytes, .. }) => assert_eq!(bytes, &vec![1, 0, 2, 0, 3, 0]),
            other => panic!("{other:?}"),
        }
        let Body::Multipart(parts) = &spec.body else { panic!() };
        assert_eq!(parts.last().unwrap().name, "file");
        let sent: Vec<_> = parts.iter().filter(|p| p.name == "keyterms").map(|p| &p.value).collect();
        assert_eq!(sent, [&PartValue::Text("TEKSOFT".into()), &PartValue::Text("Claude Code".into())]);
    }

    #[test]
    fn batch_without_language_lets_provider_detect() {
        let spec = ElevenLabsBatch.build(&SttRequest { pcm: &[], model: "m", language: None, keyterms: &[] }, "k");
        assert_eq!(spec.part("language_code"), None);
        assert_eq!(spec.part("keyterms"), None);
    }

    #[test]
    fn parses_recorded_batch_response() {
        let body = include_bytes!("../../tests/fixtures/elevenlabs_batch.json");
        let text = ElevenLabsBatch.parse(body).unwrap();
        assert!(text.to_lowercase().contains("popraw w pliku package"), "{text}");
    }

    #[test]
    fn realtime_url_and_frames() {
        let mut p = ElevenLabsLive::new("K", "scribe_v2_realtime", Some("pl"), vec!["TEKSOFT".into(), "Claude Code".into()]);
        let url = p.url();
        assert!(url.starts_with(REALTIME_URL));
        for q in [
            "model_id=scribe_v2_realtime",
            "audio_format=pcm_16000",
            "commit_strategy=manual",
            "language_code=pl",
            "keyterms=TEKSOFT&keyterms=Claude+Code",
        ] {
            assert!(url.contains(q), "{url}");
        }
        assert_eq!(p.headers(), vec![("xi-api-key".to_string(), "K".to_string())]);
        let WsOut::Text(frame) = p.audio_frame(&[1, -1]) else { panic!() };
        let v: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["message_type"], "input_audio_chunk");
        assert_eq!(v["audio_base_64"], "AQD//w==");
        assert_eq!(v["commit"], false);
        assert_eq!(v["sample_rate"], 16000);
        let fin = p.finish_frames();
        let WsOut::Text(f) = &fin[0] else { panic!() };
        let v: Value = serde_json::from_str(f).unwrap();
        assert_eq!((v["commit"].as_bool(), v["audio_base_64"].as_str()), (Some(true), Some("")));
    }

    #[test]
    fn replays_recorded_realtime_session() {
        let mut p = ElevenLabsLive::new("K", "scribe_v2_realtime", Some("pl"), vec![]);
        let mut events = Vec::new();
        let log = include_str!("../../tests/fixtures/elevenlabs_realtime.jsonl");
        let mut lines = log.lines().filter(|l| !l.trim().is_empty()).peekable();
        while let Some(line) = lines.next() {
            if lines.peek().is_none() {
                p.finish_frames();
            }
            events.push(p.handle(line));
        }
        assert_eq!(events.first(), Some(&LiveEvent::Started));
        assert_eq!(events.last(), Some(&LiveEvent::Done));
        assert!(p.transcript().to_lowercase().contains("popraw w pliku package"), "{}", p.transcript());
    }

    #[test]
    fn maps_errors() {
        let mut p = ElevenLabsLive::new("K", "m", None, vec![]);
        assert!(matches!(
            p.handle(r#"{"message_type":"auth_error","error":"bad"}"#),
            LiveEvent::Error(Error::Unauthorized { .. })
        ));
        assert!(matches!(
            p.handle(r#"{"message_type":"quota_exceeded","error":"x"}"#),
            LiveEvent::Error(Error::QuotaExceeded { .. })
        ));
        assert!(matches!(
            p.handle(r#"{"message_type":"rate_limited","error":"x"}"#),
            LiveEvent::Error(Error::RateLimited { .. })
        ));
        assert!(matches!(
            p.handle(r#"{"message_type":"transcriber_error","error":"x"}"#),
            LiveEvent::Error(Error::Provider(_))
        ));
        assert_eq!(p.handle("not json"), LiveEvent::Ignore);
        assert_eq!(p.handle(r#"{"message_type":"warning","warning":"w"}"#), LiveEvent::Ignore);
    }
}

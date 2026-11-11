//! xAI Grok speech-to-text: batch `POST /v1/stt` and the streaming WebSocket
//! `wss://api.x.ai/v1/stt` (binary PCM frames, `audio.done` on key release).

use serde_json::Value;

use super::live::{append_segment, LiveEvent, LiveProtocol, WsOut};
use super::{text_field, BatchProvider, HttpSpec, Part, PartValue, SttRequest};
use crate::audio::{pcm_to_le_bytes, TARGET_RATE};
use crate::error::{Error, Result};

pub const BATCH_URL: &str = "https://api.x.ai/v1/stt";
pub const REALTIME_URL: &str = "wss://api.x.ai/v1/stt";

pub struct XaiBatch;

impl BatchProvider for XaiBatch {
    fn build(&self, req: &SttRequest<'_>, api_key: &str) -> HttpSpec {
        let mut parts = vec![
            Part::text("model", req.model),
            Part::text("audio_format", "pcm"),
            Part::text("sample_rate", TARGET_RATE.to_string()),
        ];
        if let Some(lang) = req.language {
            parts.push(Part::text("language", lang));
        }
        // xAI ignores fields sent after `file`, so it must be the last part.
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
            headers: vec![("Authorization".into(), format!("Bearer {api_key}"))],
            body: super::Body::Multipart(parts),
        }
    }

    fn parse(&self, body: &[u8]) -> Result<String> {
        text_field(body, "text")
    }
}

pub struct XaiLive {
    api_key: String,
    model: String,
    language: Option<String>,
    finals: String,
    done_text: String,
}

impl XaiLive {
    pub fn new(api_key: &str, model: &str, language: Option<&str>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            language: language.map(str::to_string),
            finals: String::new(),
            done_text: String::new(),
        }
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

impl LiveProtocol for XaiLive {
    fn name(&self) -> &'static str {
        "xAI"
    }

    fn url(&self) -> String {
        let mut url = url::Url::parse(REALTIME_URL).expect("static URL");
        {
            let mut q = url.query_pairs_mut();
            if !self.model.is_empty() {
                q.append_pair("model", &self.model);
            }
            q.append_pair("sample_rate", &TARGET_RATE.to_string());
            q.append_pair("encoding", "pcm");
            q.append_pair("interim_results", "true");
            if let Some(lang) = &self.language {
                q.append_pair("language", lang);
            }
        }
        url.to_string()
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![("Authorization".into(), format!("Bearer {}", self.api_key))]
    }

    fn wait_for_start(&self) -> bool {
        // Docs: wait for `transcript.created` before sending audio.
        true
    }

    fn audio_frame(&mut self, pcm: &[i16]) -> WsOut {
        WsOut::Binary(pcm_to_le_bytes(pcm))
    }

    fn finish_frames(&mut self) -> Vec<WsOut> {
        vec![WsOut::Text(r#"{"type":"audio.done"}"#.into())]
    }

    fn handle(&mut self, text: &str) -> LiveEvent {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return LiveEvent::Ignore;
        };
        let text_of = |v: &Value| v.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "transcript.created" => LiveEvent::Started,
            "transcript.partial" => {
                if v.get("is_final").and_then(Value::as_bool).unwrap_or(false) {
                    append_segment(&mut self.finals, &text_of(&v));
                    LiveEvent::Final
                } else {
                    LiveEvent::Partial(text_of(&v))
                }
            }
            "transcript.done" => {
                self.done_text = text_of(&v);
                LiveEvent::Done
            }
            "error" => {
                let msg = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("nieznany błąd")
                    .to_string();
                let lower = msg.to_lowercase();
                LiveEvent::Error(if lower.contains("auth") || lower.contains("api key") {
                    Error::Unauthorized { status: 401, message: msg }
                } else if lower.contains("rate") || lower.contains("quota") || lower.contains("credit") {
                    Error::RateLimited { status: 429, message: msg }
                } else {
                    Error::Provider(msg)
                })
            }
            _ => LiveEvent::Ignore,
        }
    }

    /// Finalized chunks, completed with `transcript.done.text`. The docs leave
    /// open whether `done` carries the whole session or only the flushed tail,
    /// so both cases are handled.
    fn transcript(&self) -> String {
        let finals = self.finals.trim();
        let done = self.done_text.trim();
        if done.is_empty() {
            return finals.to_string();
        }
        if finals.is_empty() {
            return done.to_string();
        }
        let (f, d) = (squash(finals), squash(done));
        if d.starts_with(&f) {
            done.to_string()
        } else if f.ends_with(&d) {
            finals.to_string()
        } else {
            format!("{finals} {done}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::Body;

    #[test]
    fn batch_puts_file_last_and_declares_raw_pcm() {
        let spec = XaiBatch.build(
            &SttRequest { pcm: &[5], model: "grok-voice-transcribe-2.0", language: Some("pl"), keyterms: &[] },
            "K",
        );
        assert_eq!(spec.url, BATCH_URL);
        assert_eq!(spec.header("authorization"), Some("Bearer K"));
        let Body::Multipart(parts) = &spec.body else { panic!() };
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["model", "audio_format", "sample_rate", "language", "file"]);
        assert_eq!(spec.part("sample_rate"), Some(&PartValue::Text("16000".into())));
    }

    #[test]
    fn parses_documented_batch_response() {
        let body = br#"{"text":"Saldo wynosi 167 983,15 zl.","language":"pl","duration":3.45,"words":[{"text":"Saldo","start":0.24,"end":0.48}]}"#;
        assert_eq!(XaiBatch.parse(body).unwrap(), "Saldo wynosi 167 983,15 zl.");
    }

    #[test]
    fn realtime_url_binary_audio_and_done() {
        let mut p = XaiLive::new("K", "grok-voice-transcribe-2.0", Some("pl"));
        let url = p.url();
        for q in ["model=grok-voice-transcribe-2.0", "sample_rate=16000", "encoding=pcm", "interim_results=true", "language=pl"] {
            assert!(url.contains(q), "{url}");
        }
        assert!(p.wait_for_start());
        assert_eq!(p.audio_frame(&[1, -1]), WsOut::Binary(vec![1, 0, 255, 255]));
        assert_eq!(p.finish_frames(), vec![WsOut::Text(r#"{"type":"audio.done"}"#.into())]);
    }

    #[test]
    fn replays_documented_session_shape() {
        let mut p = XaiLive::new("K", "m", None);
        assert_eq!(p.handle(r#"{"type":"transcript.created","id":"s1"}"#), LiveEvent::Started);
        assert_eq!(
            p.handle(r#"{"type":"transcript.partial","text":"Napraw","is_final":false,"speech_final":false}"#),
            LiveEvent::Partial("Napraw".into())
        );
        assert_eq!(
            p.handle(r#"{"type":"transcript.partial","text":"Napraw test logowania.","is_final":true,"speech_final":false}"#),
            LiveEvent::Final
        );
        assert_eq!(
            p.handle(r#"{"type":"transcript.partial","text":"I dodaj log.","is_final":true,"speech_final":true}"#),
            LiveEvent::Final
        );
        assert_eq!(p.handle(r#"{"type":"transcript.done","text":"","words":[],"duration":2.1}"#), LiveEvent::Done);
        assert_eq!(p.transcript(), "Napraw test logowania. I dodaj log.");
    }

    #[test]
    fn done_text_covering_whole_session_wins_and_tail_is_appended() {
        let mut p = XaiLive::new("K", "m", None);
        p.handle(r#"{"type":"transcript.partial","text":"Ala ma","is_final":true}"#);
        p.handle(r#"{"type":"transcript.done","text":"Ala ma kota."}"#);
        assert_eq!(p.transcript(), "Ala ma kota.");

        let mut q = XaiLive::new("K", "m", None);
        q.handle(r#"{"type":"transcript.partial","text":"Ala ma","is_final":true}"#);
        q.handle(r#"{"type":"transcript.done","text":"kota."}"#);
        assert_eq!(q.transcript(), "Ala ma kota.");

        let mut r = XaiLive::new("K", "m", None);
        r.handle(r#"{"type":"transcript.partial","text":"Ala ma kota.","is_final":true}"#);
        r.handle(r#"{"type":"transcript.done","text":"kota."}"#);
        assert_eq!(r.transcript(), "Ala ma kota.");
    }

    #[test]
    fn maps_errors() {
        let mut p = XaiLive::new("K", "m", None);
        assert!(matches!(
            p.handle(r#"{"type":"error","message":"Invalid API key"}"#),
            LiveEvent::Error(Error::Unauthorized { .. })
        ));
        assert!(matches!(
            p.handle(r#"{"type":"error","message":"boom"}"#),
            LiveEvent::Error(Error::Provider(_))
        ));
    }
}

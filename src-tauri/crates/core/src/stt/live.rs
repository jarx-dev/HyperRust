//! Streaming (WebSocket) transcription: a provider-agnostic runner driving a
//! per-provider [`LiveProtocol`].
//!
//! Audio arrives on an unbounded channel from the microphone, so whatever is
//! recorded while the socket is still connecting simply queues up and is sent
//! the moment the session is ready: no opening words are lost. After the
//! channel closes (key released) the runner sends the provider's finish frames
//! and waits only for the final transcript.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http::HeaderValue, Message};

use crate::audio::TARGET_RATE;
use crate::error::{Error, Result};

/// Frame sent to the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsOut {
    Text(String),
    Binary(Vec<u8>),
}

/// Interpretation of one text frame received from the provider.
#[derive(Debug)]
pub enum LiveEvent {
    /// Session ready for audio.
    Started,
    /// Interim text (for logs / future live preview).
    Partial(String),
    /// A finalized segment was appended to the transcript.
    Final,
    /// The transcript requested by the finish frames is complete.
    Done,
    Error(Error),
    Ignore,
}

impl PartialEq for LiveEvent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (LiveEvent::Partial(a), LiveEvent::Partial(b)) => a == b,
            // `Error` has no PartialEq (it wraps foreign errors); compare messages.
            (LiveEvent::Error(a), LiveEvent::Error(b)) => a.to_string() == b.to_string(),
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

pub trait LiveProtocol: Send {
    fn name(&self) -> &'static str;
    fn url(&self) -> String;
    fn headers(&self) -> Vec<(String, String)>;
    /// Whether audio must wait for [`LiveEvent::Started`].
    fn wait_for_start(&self) -> bool {
        false
    }
    fn start_frames(&mut self) -> Vec<WsOut> {
        Vec::new()
    }
    /// Encode a chunk of 16 kHz mono PCM16.
    fn audio_frame(&mut self, pcm: &[i16]) -> WsOut;
    /// Frames that end the audio and request the final transcript.
    fn finish_frames(&mut self) -> Vec<WsOut>;
    fn handle(&mut self, text: &str) -> LiveEvent;
    /// Transcript assembled so far.
    fn transcript(&self) -> String;
}

/// Audio is sent in frames of at least this many samples (100 ms)…
const MIN_FRAME: usize = TARGET_RATE as usize / 10;
/// …and at most this many (250 ms), e.g. when draining the backlog after connecting.
const MAX_FRAME: usize = TARGET_RATE as usize / 4;
const START_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A frame that cannot be written in this time means a stalled connection.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LiveOptions {
    /// Wait this long for the final transcript after the audio ended.
    pub final_timeout: Duration,
    /// Fired once the socket is open and ready for audio.
    pub on_connected: Option<oneshot::Sender<Instant>>,
}

/// Run one streaming session until the audio channel closes and the final
/// transcript arrives.
pub async fn run_live(
    mut proto: Box<dyn LiveProtocol>,
    mut audio: mpsc::UnboundedReceiver<Vec<i16>>,
    opts: LiveOptions,
) -> Result<String> {
    let mut request = proto
        .url()
        .into_client_request()
        .map_err(|e| Error::Other(format!("URL WebSocket: {e}")))?;
    for (k, v) in proto.headers() {
        let value = HeaderValue::from_str(&v).map_err(|e| Error::Other(e.to_string()))?;
        let name = tungstenite::http::HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| Error::Other(e.to_string()))?;
        request.headers_mut().insert(name, value);
    }

    let (mut ws, _resp) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| Error::Timeout(CONNECT_TIMEOUT))?
        .map_err(map_ws_error)?;

    for frame in proto.start_frames() {
        send(&mut ws, frame).await?;
    }

    if proto.wait_for_start() {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            let msg = tokio::time::timeout_at(deadline, ws.next())
                .await
                .map_err(|_| Error::Timeout(START_TIMEOUT))?;
            match next_event(&mut *proto, msg)? {
                Some(LiveEvent::Started) => break,
                Some(LiveEvent::Error(e)) => return Err(e),
                Some(_) => {}
                None => return Err(Error::Network(crate::tr!("connection closed before the session started", "połączenie zamknięte przed startem sesji"))),
            }
        }
    }
    if let Some(tx) = opts.on_connected {
        let _ = tx.send(Instant::now());
    }

    // Stream audio while reading provider events.
    let mut pending: Vec<i16> = Vec::with_capacity(MAX_FRAME * 2);
    loop {
        tokio::select! {
            chunk = audio.recv() => match chunk {
                Some(c) => {
                    pending.extend_from_slice(&c);
                    while pending.len() >= MIN_FRAME {
                        let n = pending.len().min(MAX_FRAME);
                        let frame = proto.audio_frame(&pending[..n]);
                        pending.drain(..n);
                        send(&mut ws, frame).await?;
                    }
                }
                None => break,
            },
            msg = ws.next() => match next_event(&mut *proto, msg)? {
                Some(LiveEvent::Error(e)) => return Err(e),
                Some(_) => {}
                None => return Err(Error::Network(crate::tr!("the provider closed the connection during the recording", "dostawca zamknął połączenie w trakcie nagrania"))),
            },
        }
    }
    if !pending.is_empty() {
        let frame = proto.audio_frame(&pending);
        send(&mut ws, frame).await?;
    }
    for frame in proto.finish_frames() {
        send(&mut ws, frame).await?;
    }

    // Wait for the final transcript. Without the protocol's end event the text
    // may miss its tail, so the caller re-sends the recording as a batch.
    let deadline = tokio::time::Instant::now() + opts.final_timeout;
    let result = loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => break Err(Error::Timeout(opts.final_timeout)),
            Ok(msg) => match next_event(&mut *proto, msg)? {
                Some(LiveEvent::Done) => break Ok(proto.transcript()),
                Some(LiveEvent::Error(e)) => break Err(e),
                Some(_) => {}
                None => break Err(Error::Network(crate::tr!("connection closed before the final transcript", "połączenie zamknięte przed końcowym transkryptem"))),
            },
        }
    };
    // Close in the background; the caller must not wait for the handshake.
    tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
    });
    result
}

fn to_message(frame: WsOut) -> Message {
    match frame {
        WsOut::Text(t) => Message::Text(t.into()),
        WsOut::Binary(b) => Message::Binary(b.into()),
    }
}

async fn send<S>(ws: &mut S, frame: WsOut) -> Result<()>
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Unpin,
{
    tokio::time::timeout(SEND_TIMEOUT, ws.send(to_message(frame)))
        .await
        .map_err(|_| Error::Timeout(SEND_TIMEOUT))?
        .map_err(map_ws_error)
}

/// `None` = the socket is closed.
fn next_event(
    proto: &mut dyn LiveProtocol,
    msg: Option<std::result::Result<Message, tungstenite::Error>>,
) -> Result<Option<LiveEvent>> {
    match msg {
        None => Ok(None),
        Some(Err(e)) => Err(map_ws_error(e)),
        Some(Ok(Message::Text(t))) => Ok(Some(proto.handle(t.as_str()))),
        Some(Ok(Message::Binary(b))) => Ok(Some(match std::str::from_utf8(&b) {
            Ok(t) => proto.handle(t),
            Err(_) => LiveEvent::Ignore,
        })),
        Some(Ok(Message::Close(frame))) => {
            if let Some(f) = frame {
                let code = u16::from(f.code);
                if code != 1000 && !f.reason.is_empty() {
                    tracing::warn!("{}: zamknięcie {code}: {}", proto.name(), f.reason);
                }
            }
            Ok(None)
        }
        Some(Ok(_)) => Ok(Some(LiveEvent::Ignore)),
    }
}

fn map_ws_error(e: tungstenite::Error) -> Error {
    match e {
        tungstenite::Error::Http(resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default();
            Error::from_status(status, &body)
        }
        other => Error::Network(other.to_string()),
    }
}

/// Base64 of little-endian PCM16.
pub(crate) fn pcm_base64(pcm: &[i16]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(crate::audio::pcm_to_le_bytes(pcm))
}

/// Append a finalized segment with a separating space.
pub(crate) fn append_segment(acc: &mut String, segment: &str) {
    let seg = segment.trim();
    if seg.is_empty() {
        return;
    }
    if !acc.is_empty() {
        acc.push(' ');
    }
    acc.push_str(seg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_join_with_single_spaces() {
        let mut s = String::new();
        append_segment(&mut s, "  Ala ma ");
        append_segment(&mut s, "");
        append_segment(&mut s, "kota.");
        assert_eq!(s, "Ala ma kota.");
    }

    #[test]
    fn base64_is_little_endian() {
        assert_eq!(pcm_base64(&[1, -1]), "AQD//w==");
    }
}

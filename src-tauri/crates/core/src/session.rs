//! One dictation, minus the OS: audio chunks in, transcript (and optional
//! post-processed text) out. Shared by the app and the headless smoke test.

use std::time::{Duration, Instant};

use reqwest::Client;
use tokio::sync::{mpsc, oneshot};

use crate::audio::{contains_speech, TARGET_RATE};
use crate::catalog;
use crate::config::{LlmBackend, LlmSettings, SttSettings};
use crate::dictionary::DictionaryEntry;
use crate::error::{Error, Result};
use crate::llm::{self, CliKind, LlmOutput};
use crate::modes::Mode;
use crate::stt::{self, live, SttProviderKind, SttRequest};

#[derive(Debug, Clone)]
pub struct SttOutcome {
    pub text: String,
    /// The transcript came from the streaming session (no batch fallback).
    pub streamed: bool,
    /// Why streaming was abandoned for a batch request, if it was.
    pub fallback_reason: Option<String>,
    pub audio_samples: usize,
    pub connected_at: Option<Instant>,
    /// USD the provider reported for a batch request (OpenRouter does).
    pub cost_usd: Option<f64>,
    /// Position in the keys given to [`transcribe`] of the key that worked.
    pub key_index: usize,
}

/// Extra time, on top of the final-transcript timeout, for flushing the audio
/// backlog to the socket after the key release.
const LIVE_FLUSH_GRACE: Duration = Duration::from_secs(3);

/// Aborts the streaming task when dropped, so a cancelled dictation (Escape,
/// a too-short tap, a newer recording) also stops sending audio upstream.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Collect the recording from `input` (closed on key release) and transcribe
/// it, streaming while the user speaks when the provider supports it and
/// falling back to a batch request if the stream fails.
///
/// `api_keys` are accounts of the same provider, the preferred one first: it
/// streams, and when an account refuses its key or has no credits left (see
/// [`Error::key_unusable`]) the recording goes to the next one as a batch request.
pub async fn transcribe(
    client: &Client,
    stt_settings: &SttSettings,
    api_keys: &[String],
    mut input: mpsc::UnboundedReceiver<Vec<i16>>,
    min_samples: usize,
) -> Result<SttOutcome> {
    let Some(first_key) = api_keys.first() else {
        return Err(Error::MissingKey(stt_settings.provider.label()));
    };
    let mut connected_rx: Option<oneshot::Receiver<Instant>> = None;
    let mut live_tx: Option<mpsc::UnboundedSender<Vec<i16>>> = None;
    let mut live_task = None;

    if stt_settings.streaming {
        if let Some(proto) = stt::live_protocol(stt_settings, first_key) {
            let (tx, rx) = mpsc::unbounded_channel();
            let (ctx, crx) = oneshot::channel();
            let opts = live::LiveOptions {
                final_timeout: Duration::from_millis(stt_settings.final_timeout_ms),
                on_connected: Some(ctx),
            };
            live_task = Some(AbortOnDrop(tokio::spawn(live::run_live(proto, rx, opts))));
            live_tx = Some(tx);
            connected_rx = Some(crx);
        }
    }

    let mut audio: Vec<i16> = Vec::with_capacity(TARGET_RATE as usize * 30);
    while let Some(chunk) = input.recv().await {
        if let Some(tx) = &live_tx {
            if tx.send(chunk.clone()).is_err() {
                live_tx = None; // the stream already failed; keep recording for batch
            }
        }
        audio.extend_from_slice(&chunk);
    }
    drop(live_tx);

    let connected_at = connected_rx.and_then(|mut rx| rx.try_recv().ok());
    if audio.len() < min_samples || !contains_speech(&audio, TARGET_RATE) {
        return Err(Error::NoSpeech); // dropping `live_task` aborts the stream
    }

    let mut fallback_reason = None;
    let mut key_index = 0;
    if let Some(mut task) = live_task {
        let limit = Duration::from_millis(stt_settings.final_timeout_ms) + LIVE_FLUSH_GRACE;
        match tokio::time::timeout(limit, &mut task.0).await {
            Ok(Ok(Ok(text))) if !text.trim().is_empty() => {
                return Ok(SttOutcome {
                    text: text.trim().to_string(),
                    streamed: true,
                    fallback_reason: None,
                    audio_samples: audio.len(),
                    connected_at,
                    cost_usd: None,
                    key_index,
                });
            }
            Ok(Ok(Ok(_))) => fallback_reason = Some("pusty wynik strumienia".to_string()),
            Ok(Ok(Err(e))) => {
                // The account that refused the stream would refuse the batch too.
                if e.key_unusable() && api_keys.len() > 1 {
                    key_index = 1;
                }
                fallback_reason = Some(e.to_string());
            }
            Ok(Err(e)) => fallback_reason = Some(format!("zadanie strumienia: {e}")),
            Err(_) => fallback_reason = Some(Error::Timeout(limit).to_string()),
        }
        tracing::warn!(
            "{}: streaming nieudany ({}), wysyłam nagranie wsadowo",
            stt_settings.provider.label(),
            fallback_reason.as_deref().unwrap_or("")
        );
    }

    let (text, cost_usd) = loop {
        match transcribe_once(client, stt_settings, &api_keys[key_index], &audio).await {
            Err(e) if e.key_unusable() && key_index + 1 < api_keys.len() => {
                tracing::warn!("{}: klucz nr {} nie działa ({e}), próbuję następnego", stt_settings.provider.label(), key_index + 1);
                key_index += 1;
            }
            result => break result?,
        }
    };
    Ok(SttOutcome { text, streamed: false, fallback_reason, audio_samples: audio.len(), connected_at, cost_usd, key_index })
}

/// Batch transcription of a complete recording, with the billed cost when
/// the provider reports it.
pub async fn transcribe_once(
    client: &Client,
    stt_settings: &SttSettings,
    api_key: &str,
    audio: &[i16],
) -> Result<(String, Option<f64>)> {
    let provider = stt::batch_provider(stt_settings.provider);
    let audio_secs = audio.len() as u64 / TARGET_RATE as u64;
    let timeout = Duration::from_millis(stt_settings.final_timeout_ms.max(15_000)) + Duration::from_secs(audio_secs);
    let keyterms = crate::dictionary::keyterms(&stt_settings.dictionary);
    let req = SttRequest {
        pcm: audio,
        model: stt_settings.batch_model(),
        language: stt_settings.language(),
        keyterms: &keyterms,
    };
    let (text, cost) = stt::transcribe_batch(client, provider.as_ref(), &req, api_key, timeout).await?;
    if text.trim().is_empty() {
        return Err(Error::NoSpeech);
    }
    Ok((text, cost))
}

/// What the transcript cost in USD and whether that is an estimate: billed by
/// the provider when the response says, else the built-in price list times
/// the recording.
pub fn stt_cost(stt_settings: &SttSettings, outcome: &SttOutcome) -> Option<(f64, bool)> {
    if let Some(usd) = outcome.cost_usd {
        return Some((usd, false));
    }
    let model = if outcome.streamed { stt_settings.realtime_model() } else { stt_settings.batch_model() };
    let mut per_hour = catalog::stt_hourly_price(stt_settings.provider, model, outcome.streamed)?;
    // ElevenLabs keyterms: +20% on realtime, +0.05 $/h on batch (price list of 26.10.2026).
    if stt_settings.provider == SttProviderKind::ElevenLabs && !crate::dictionary::keyterms(&stt_settings.dictionary).is_empty() {
        per_hour = if outcome.streamed { per_hour * 1.2 } else { per_hour + 0.05 };
    }
    Some((per_hour * outcome.audio_samples as f64 / TARGET_RATE as f64 / 3600.0, true))
}

/// Human-readable name of the post-processing target, for logs.
pub fn llm_label(llm_settings: &LlmSettings) -> String {
    match llm_settings.backend {
        LlmBackend::OpenAiCompatible => format!("endpoint:{}", llm_settings.model),
        LlmBackend::OpenRouter => format!("openrouter:{}", llm_settings.openrouter_model),
        LlmBackend::ClaudeCli => format!("claude-cli:{}", llm_settings.cli_model),
        LlmBackend::CodexCli => format!("codex-cli:{}", llm_settings.cli_model),
    }
}

/// Run the mode's post-processing over a transcript, with the user's
/// dictionary appended to the system prompt.
pub async fn post_process(
    client: &Client,
    llm_settings: &LlmSettings,
    mode: &Mode,
    dictionary: &[DictionaryEntry],
    transcript: &str,
    api_key: Option<&str>,
) -> Result<LlmOutput> {
    let timeout = Duration::from_millis(llm_settings.timeout_ms);
    let base = mode.system_prompt();
    let full = format!("{base}{}", crate::dictionary::prompt_hint(dictionary));
    let system = full.as_str();
    match llm_settings.backend {
        LlmBackend::ClaudeCli => llm::complete_cli(CliKind::Claude, &llm_settings.cli_model, system, transcript, timeout).await,
        LlmBackend::CodexCli => llm::complete_cli(CliKind::Codex, &llm_settings.cli_model, system, transcript, timeout).await,
        LlmBackend::OpenAiCompatible | LlmBackend::OpenRouter => {
            let (base, model) = llm_settings.http_target().expect("http backend");
            if base.is_empty() || model.is_empty() {
                return Err(Error::Other(crate::tr!("set the post-processing endpoint address and model", "ustaw adres i model endpointu post-processingu")));
            }
            if llm_settings.backend == LlmBackend::OpenRouter && api_key.is_none_or(|k| k.trim().is_empty()) {
                return Err(Error::MissingKey("OpenRouter"));
            }
            // The built-in cleanup prompt is recognised without the dictionary part.
            let effort = llm::effective_effort(&llm_settings.reasoning_effort, model, base);
            let body = llm::chat_body(model, system, transcript, effort);
            llm::complete_http(client, base, api_key, &body, timeout).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_cost_prefers_the_billed_amount() {
        let stt = SttSettings { provider: SttProviderKind::ElevenLabs, ..Default::default() };
        let minute = TARGET_RATE as usize * 60;
        let mut outcome = SttOutcome {
            text: "x".into(),
            streamed: true,
            fallback_reason: None,
            audio_samples: minute,
            connected_at: None,
            cost_usd: None,
            key_index: 0,
        };
        // scribe_v2_realtime: 0.39 $ per hour, +20% for the default dictionary's keyterms.
        let (usd, estimated) = stt_cost(&stt, &outcome).unwrap();
        assert!((usd - 0.0078).abs() < 1e-9 && estimated);
        let no_dictionary = SttSettings { dictionary: vec![], ..stt.clone() };
        assert!((stt_cost(&no_dictionary, &outcome).unwrap().0 - 0.0065).abs() < 1e-9);
        outcome.cost_usd = Some(0.000975);
        assert_eq!(stt_cost(&stt, &outcome), Some((0.000975, false)));
    }
}

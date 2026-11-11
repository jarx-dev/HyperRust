//! Headless smoke test of the dictation path on any OS, without a GUI.
//!
//! Plays a WAV file into the pipeline at real-time pace (as if spoken while the
//! key is held), "releases the key" at the end of the audio, then measures
//! release → transcript → post-processing exactly like the app logs it.
//!
//! ```text
//! ELEVENLABS_API_KEY=… CLIPROXY_API_KEY=… cargo run -p hyperrust-core --example smoke -- \
//!     --wav nagranie.wav --provider elevenlabs [--batch] [--no-llm] [--fast] [--runs 3] \
//!     [--llm-backend endpoint|openrouter|claude|codex] [--llm-model M] [--llm-base URL] \
//!     [--record-frames frames.jsonl]
//! ```
//! `--record-frames` runs only the streaming session and saves every frame the
//! provider sent (one per line), which is how the realtime test fixtures are made.
//! Keys come from environment variables only (see `SecretKey::env_var`);
//! `ELEVENLABS_BACKUP_API_KEY` is the second account's key the app moves to
//! when the first one fails.

use std::time::{Duration, Instant};

use hyperrust_core::audio::{parse_wav, resample_i16, TARGET_RATE};
use hyperrust_core::config::{LlmBackend, SecretKey, Settings};
use hyperrust_core::http::build_client;
use hyperrust_core::modes::RAW_MODE_ID;
use hyperrust_core::session::{llm_label, post_process, stt_cost, transcribe};
use hyperrust_core::stt::live::{self, LiveEvent, LiveOptions, LiveProtocol, WsOut};
use hyperrust_core::stt::{self as stt_mod, SttProviderKind};
use hyperrust_core::timing::{Timeline, TimingMeta};
use tokio::sync::mpsc;

struct Args {
    wav: String,
    provider: SttProviderKind,
    streaming: bool,
    llm: bool,
    realtime: bool,
    runs: usize,
    llm_backend: LlmBackend,
    llm_model: Option<String>,
    llm_base: Option<String>,
    stt_model: Option<String>,
    record_frames: Option<String>,
}

/// Wraps a live protocol and appends every received frame to a file.
struct Recorder {
    inner: Box<dyn LiveProtocol>,
    out: std::fs::File,
}

impl LiveProtocol for Recorder {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn url(&self) -> String {
        self.inner.url()
    }
    fn headers(&self) -> Vec<(String, String)> {
        self.inner.headers()
    }
    fn wait_for_start(&self) -> bool {
        self.inner.wait_for_start()
    }
    fn start_frames(&mut self) -> Vec<WsOut> {
        self.inner.start_frames()
    }
    fn audio_frame(&mut self, pcm: &[i16]) -> WsOut {
        self.inner.audio_frame(pcm)
    }
    fn finish_frames(&mut self) -> Vec<WsOut> {
        self.inner.finish_frames()
    }
    fn handle(&mut self, text: &str) -> LiveEvent {
        use std::io::Write;
        let _ = writeln!(self.out, "{}", text.replace('\n', " "));
        self.inner.handle(text)
    }
    fn transcript(&self) -> String {
        self.inner.transcript()
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        wav: String::new(),
        provider: SttProviderKind::ElevenLabs,
        streaming: true,
        llm: true,
        realtime: true,
        runs: 1,
        llm_backend: LlmBackend::default(),
        llm_model: None,
        llm_base: None,
        stt_model: None,
        record_frames: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--wav" => a.wav = it.next().expect("--wav PATH"),
            "--provider" => {
                a.provider = serde_json::from_value(serde_json::Value::String(it.next().expect("--provider")))
                    .expect("provider: elevenlabs|xai|openai|openrouter")
            }
            "--batch" => a.streaming = false,
            "--no-llm" => a.llm = false,
            "--fast" => a.realtime = false,
            "--runs" => a.runs = it.next().and_then(|n| n.parse().ok()).expect("--runs N"),
            "--stt-model" => a.stt_model = it.next(),
            "--record-frames" => a.record_frames = it.next(),
            "--llm-model" => a.llm_model = it.next(),
            "--llm-base" => a.llm_base = it.next(),
            "--llm-backend" => {
                a.llm_backend = match it.next().as_deref() {
                    Some("endpoint") => LlmBackend::OpenAiCompatible,
                    Some("openrouter") => LlmBackend::OpenRouter,
                    Some("claude") => LlmBackend::ClaudeCli,
                    Some("codex") => LlmBackend::CodexCli,
                    other => panic!("nieznany backend: {other:?}"),
                }
            }
            other => panic!("nieznany argument: {other}"),
        }
    }
    assert!(!a.wav.is_empty(), "podaj --wav PLIK");
    a
}

fn env_key(k: SecretKey) -> Option<String> {
    std::env::var(k.env_var()).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let wav = parse_wav(&std::fs::read(&args.wav).expect("odczyt WAV")).expect("WAV PCM16");
    let pcm = resample_i16(&wav.samples, wav.sample_rate, TARGET_RATE);
    let client = build_client().expect("klient HTTP");

    let mut settings = Settings::default();
    settings.stt.provider = args.provider;
    settings.stt.streaming = args.streaming;
    if let Some(m) = &args.stt_model {
        match args.provider {
            SttProviderKind::ElevenLabs => settings.stt.models.elevenlabs = m.clone(),
            SttProviderKind::Xai => settings.stt.models.xai = m.clone(),
            SttProviderKind::OpenAi => settings.stt.models.openai = m.clone(),
            SttProviderKind::OpenRouter => settings.stt.models.openrouter = m.clone(),
        }
    }
    settings.llm.backend = args.llm_backend;
    if let Some(m) = &args.llm_model {
        match args.llm_backend {
            LlmBackend::OpenRouter => settings.llm.openrouter_model = m.clone(),
            LlmBackend::OpenAiCompatible => settings.llm.model = m.clone(),
            _ => settings.llm.cli_model = m.clone(),
        }
    }
    if let Some(b) = &args.llm_base {
        settings.llm.base_url = b.clone();
    }
    if !args.llm {
        settings.active_mode = RAW_MODE_ID.into();
    }
    let mode = settings.active_mode().clone();
    let stt_keys: Vec<String> = args.provider.secrets().into_iter().filter_map(env_key).collect();
    let stt_key = stt_keys
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("brak klucza w zmiennej {}", args.provider.secret().env_var()));
    let llm_key = settings.llm.secret().and_then(env_key);

    println!(
        "plik={} audio={:.2}s stt={} streaming={} tryb={} llm={}",
        args.wav,
        pcm.len() as f64 / TARGET_RATE as f64,
        args.provider.label(),
        args.streaming,
        mode.name,
        if mode.post_process { llm_label(&settings.llm) } else { "-".into() }
    );

    if let Some(path) = &args.record_frames {
        let proto = stt_mod::live_protocol(&settings.stt, &stt_key).expect("dostawca bez streamingu");
        let out = std::fs::File::create(path).expect("plik ramek");
        let (tx, rx) = mpsc::unbounded_channel();
        let opts = LiveOptions { final_timeout: Duration::from_secs(10), on_connected: None };
        let task = tokio::spawn(live::run_live(Box::new(Recorder { inner: proto, out }), rx, opts));
        let start = Instant::now();
        for (i, c) in pcm.chunks(TARGET_RATE as usize / 50).enumerate() {
            tokio::time::sleep_until((start + Duration::from_millis(20 * (i as u64 + 1))).into()).await;
            let _ = tx.send(c.to_vec());
        }
        drop(tx);
        let released = Instant::now();
        let res = task.await.expect("zadanie strumienia");
        println!("ramki zapisane w {path}; po puszczeniu: {} ms; wynik: {res:?}", released.elapsed().as_millis());
        return;
    }

    for run in 1..=args.runs {
        let mut tl = Timeline::start();
        let (tx, rx) = mpsc::unbounded_channel();
        let feeder_pcm = pcm.clone();
        let realtime = args.realtime;
        let feeder = tokio::spawn(async move {
            let chunk = TARGET_RATE as usize / 50; // 20 ms, like a WASAPI callback
            let start = Instant::now();
            for (i, c) in feeder_pcm.chunks(chunk).enumerate() {
                if realtime {
                    let due = start + Duration::from_millis(20 * (i as u64 + 1));
                    tokio::time::sleep_until(due.into()).await;
                }
                if tx.send(c.to_vec()).is_err() {
                    break;
                }
            }
            Instant::now() // key release
        });
        let stt_fut = transcribe(&client, &settings.stt, &stt_keys, rx, TARGET_RATE as usize / 4);
        let (stt_res, released) = tokio::join!(stt_fut, feeder);
        tl.released = Some(released.expect("feeder"));
        let outcome = match stt_res {
            Ok(o) => o,
            Err(e) => {
                println!("run {run}: błąd STT: {e}");
                continue;
            }
        };
        tl.stt_done = Some(Instant::now());
        tl.stt_connected = outcome.connected_at;
        let mut final_text = outcome.text.clone();
        let mut outcome_label = "ok".to_string();
        let mut llm_cost_usd = None;
        if mode.post_process {
            match post_process(&client, &settings.llm, &mode, &settings.stt.dictionary, &outcome.text, llm_key.as_deref()).await {
                Ok(out) => {
                    tl.llm_first_token = out.first_token;
                    tl.llm_done = Some(Instant::now());
                    llm_cost_usd = out.cost_usd;
                    final_text = out.text;
                }
                Err(e) => {
                    tl.llm_done = Some(Instant::now());
                    outcome_label = format!("llm_error:{e}");
                }
            }
        }
        final_text = hyperrust_core::dictionary::apply(&final_text, &settings.stt.dictionary);
        tl.pasted = Some(Instant::now());
        let stt_cost = stt_cost(&settings.stt, &outcome);
        let report = tl.report(TimingMeta {
            mode: mode.id.clone(),
            stt: args.provider.label().into(),
            streamed: outcome.streamed,
            llm: if mode.post_process { llm_label(&settings.llm) } else { String::new() },
            chars: final_text.chars().count(),
            outcome: outcome_label,
            stt_cost_usd: stt_cost.map(|(usd, _)| usd),
            stt_cost_estimated: stt_cost.is_some_and(|(_, estimated)| estimated),
            llm_cost_usd,
        });
        println!("run {run}: {report}");
        if let Some(r) = &outcome.fallback_reason {
            println!("run {run}: fallback do trybu wsadowego: {r}");
        }
        if outcome.key_index > 0 {
            println!("run {run}: użyto klucza nr {}", outcome.key_index + 1);
        }
        println!("run {run}: transkrypcja: {}", outcome.text);
        println!("run {run}: wynik:\n{final_text}");
    }
}

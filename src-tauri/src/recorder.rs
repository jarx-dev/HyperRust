//! Microphone capture (cpal/WASAPI) on a dedicated thread, converted to the
//! 16 kHz mono PCM16 every provider accepts.
//!
//! With `keep_warm` the input stream stays open between dictations, so a key
//! press costs nothing and the last `preroll_ms` of audio (the first syllable
//! spoken together with the press) is prepended to the recording.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyperrust_core::audio::TARGET_RATE;
use hyperrust_core::config::AudioSettings;
use tokio::sync::{mpsc, oneshot};

use crate::controller::CtlMsg;

#[derive(Default)]
struct Inner {
    /// Receiver of live audio while recording.
    sink: Option<mpsc::UnboundedSender<Vec<i16>>>,
    preroll: VecDeque<i16>,
    preroll_cap: usize,
}

struct Shared {
    inner: Mutex<Inner>,
    /// Input level 0..1 as f32 bits, for the overlay meter.
    level: AtomicU32,
}

enum Cmd {
    Configure(AudioSettings),
    Start(mpsc::UnboundedSender<Vec<i16>>, oneshot::Sender<Result<(), String>>),
    Stop,
    ListDevices(oneshot::Sender<Vec<String>>),
    /// Sent by the Windows capture callback when the device disappears.
    #[cfg_attr(not(windows), allow(dead_code))]
    StreamFailed(String),
}

#[derive(Clone)]
pub struct Recorder {
    cmd: std_mpsc::Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Recorder {
    /// `ctl` learns about a device failure during a recording, so the
    /// dictation is cancelled instead of pasting a truncated transcript.
    pub fn spawn(settings: AudioSettings, ctl: mpsc::UnboundedSender<CtlMsg>) -> Self {
        let shared = Arc::new(Shared { inner: Mutex::new(Inner::default()), level: AtomicU32::new(0) });
        let (tx, rx) = std_mpsc::channel();
        let thread_shared = shared.clone();
        let thread_tx = tx.clone();
        std::thread::Builder::new()
            .name("hyperrust-audio".into())
            .spawn(move || audio_thread(rx, thread_tx, thread_shared, ctl))
            .expect("audio thread");
        let rec = Self { cmd: tx, shared };
        rec.configure(settings);
        rec
    }

    pub fn configure(&self, settings: AudioSettings) {
        let _ = self.cmd.send(Cmd::Configure(settings));
    }

    /// Begin delivering audio (pre-roll first) to `sink`.
    pub async fn start(&self, sink: mpsc::UnboundedSender<Vec<i16>>) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Cmd::Start(sink, tx)).map_err(|_| "wątek audio nie działa".to_string())?;
        rx.await.map_err(|_| "wątek audio nie odpowiada".to_string())?
    }

    /// Stop delivering audio; dropping the sink ends the dictation's stream.
    pub fn stop(&self) {
        // Detach the sink immediately (not via the command queue) so no audio
        // recorded after the key release reaches the transcript.
        self.shared.inner.lock().expect("audio mutex").sink = None;
        let _ = self.cmd.send(Cmd::Stop);
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    pub async fn devices(&self) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        if self.cmd.send(Cmd::ListDevices(tx)).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

fn audio_thread(
    rx: std_mpsc::Receiver<Cmd>,
    self_tx: std_mpsc::Sender<Cmd>,
    shared: Arc<Shared>,
    ctl: mpsc::UnboundedSender<CtlMsg>,
) {
    let mut settings = AudioSettings::default();
    let mut stream: Option<backend::Stream> = None;
    let mut stream_device = String::new();
    let mut recording = false;

    loop {
        // While keep-warm is on but the stream died, retry every few seconds.
        let wants_warm = cfg!(windows) && settings.keep_warm && stream.is_none();
        let cmd = if wants_warm {
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(c) => Some(c),
                Err(std_mpsc::RecvTimeoutError::Timeout) => None,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };

        match cmd {
            None => {}
            Some(Cmd::Configure(s)) => {
                if s.device != stream_device {
                    stream = None;
                }
                shared.inner.lock().expect("audio mutex").preroll_cap = if s.keep_warm {
                    s.preroll_ms as usize * TARGET_RATE as usize / 1000
                } else {
                    0
                };
                settings = s;
                if !settings.keep_warm && !recording {
                    stream = None;
                }
            }
            Some(Cmd::Start(sink, reply)) => {
                if stream.is_none() {
                    match backend::open(&settings.device, &shared, &self_tx) {
                        Ok(s) => {
                            stream = Some(s);
                            stream_device = settings.device.clone();
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            continue;
                        }
                    }
                }
                let mut inner = shared.inner.lock().expect("audio mutex");
                let preroll: Vec<i16> = inner.preroll.drain(..).collect();
                if !preroll.is_empty() {
                    let _ = sink.send(preroll);
                }
                inner.sink = Some(sink);
                recording = true;
                let _ = reply.send(Ok(()));
            }
            Some(Cmd::Stop) => {
                recording = false;
                let mut inner = shared.inner.lock().expect("audio mutex");
                inner.sink = None;
                inner.preroll.clear();
                drop(inner);
                if !settings.keep_warm {
                    stream = None;
                }
                shared.level.store(0, Ordering::Relaxed);
            }
            Some(Cmd::ListDevices(reply)) => {
                let _ = reply.send(backend::devices());
            }
            Some(Cmd::StreamFailed(e)) => {
                tracing::warn!("strumień mikrofonu przerwany: {e}");
                stream = None;
                if recording {
                    // The controller discards the dictation (which stops us);
                    // closing the sink here would look like a key release.
                    let _ = ctl.send(CtlMsg::MicFailed(e));
                }
            }
        }

        if cfg!(windows) && settings.keep_warm && stream.is_none() {
            match backend::open(&settings.device, &shared, &self_tx) {
                Ok(s) => {
                    stream = Some(s);
                    stream_device = settings.device.clone();
                }
                Err(e) => tracing::warn!("mikrofon niedostępny: {e}"),
            }
        }
    }
}

#[cfg(windows)]
mod backend {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use hyperrust_core::audio::{downmix, f32_to_i16, meter_level, Resampler, TARGET_RATE};

    use super::{Cmd, Shared};

    pub type Stream = cpal::Stream;

    pub fn devices() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|it| it.map(|d| d.to_string()).collect())
            .unwrap_or_default()
    }

    fn find_device(name: &str) -> Result<cpal::Device, String> {
        let host = cpal::default_host();
        if !name.trim().is_empty() {
            if let Ok(mut devices) = host.input_devices() {
                if let Some(d) = devices.find(|d| d.to_string() == name) {
                    return Ok(d);
                }
            }
            tracing::warn!("nie znaleziono mikrofonu „{name}”, używam domyślnego");
        }
        host.default_input_device().ok_or_else(|| "brak mikrofonu".to_string())
    }

    pub fn open(device_name: &str, shared: &Arc<Shared>, cmd: &std_mpsc::Sender<Cmd>) -> Result<Stream, String> {
        let device = find_device(device_name)?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let config = supported.config();
        let channels = config.channels as usize;
        let rate = config.sample_rate;
        tracing::info!("mikrofon: {device} ({rate} Hz, {channels} kan., {:?})", supported.sample_format());

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, channels, shared, cmd),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, channels, shared, cmd),
            cpal::SampleFormat::I32 => build::<i32>(&device, config, channels, shared, cmd),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, channels, shared, cmd),
            other => return Err(format!("nieobsługiwany format próbek: {other:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(stream)
    }

    fn build<T>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        channels: usize,
        shared: &Arc<Shared>,
        cmd: &std_mpsc::Sender<Cmd>,
    ) -> Result<cpal::Stream, String>
    where
        T: cpal::SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let shared = shared.clone();
        let cmd = cmd.clone();
        let mut resampler = Resampler::new(config.sample_rate, TARGET_RATE);
        let mut scratch: Vec<f32> = Vec::new();
        device
            .build_input_stream::<T, _, _>(
                config,
                move |data: &[T], _| {
                    scratch.clear();
                    scratch.extend(data.iter().map(|s| cpal::Sample::to_sample::<f32>(*s)));
                    let pcm = f32_to_i16(&resampler.process(&downmix(&scratch, channels)));
                    shared.level.store(meter_level(&pcm).to_bits(), Ordering::Relaxed);
                    let mut inner = shared.inner.lock().expect("audio mutex");
                    if let Some(sink) = &inner.sink {
                        if sink.send(pcm).is_err() {
                            inner.sink = None;
                        }
                    } else if inner.preroll_cap > 0 {
                        inner.preroll.extend(pcm);
                        let excess = inner.preroll.len().saturating_sub(inner.preroll_cap);
                        inner.preroll.drain(..excess);
                    }
                },
                move |err: cpal::Error| {
                    // Overruns are reported but harmless; anything else ends the stream.
                    if err.kind() != cpal::ErrorKind::Xrun {
                        let _ = cmd.send(Cmd::StreamFailed(err.to_string()));
                    }
                },
                None,
            )
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod backend {
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc;

    use super::{Cmd, Shared};

    pub struct Stream;

    pub fn devices() -> Vec<String> {
        Vec::new()
    }

    pub fn open(_device: &str, _shared: &Arc<Shared>, _cmd: &std_mpsc::Sender<Cmd>) -> Result<Stream, String> {
        Err("nagrywanie jest obsługiwane tylko w Windows".into())
    }
}

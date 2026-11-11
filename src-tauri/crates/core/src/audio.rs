//! Audio helpers on the dictation path: down-mixing, streaming resampling to
//! the 16 kHz mono PCM16 every STT provider accepts, WAV framing, level metering
//! and a conservative "is there any speech at all" check.

/// Sample rate of the canonical dictation stream.
pub const TARGET_RATE: u32 = 16_000;

/// Average interleaved frames into mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect()
}

pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / i16::MAX as f32).collect()
}

/// Streaming mono resampler: a windowed-sinc low-pass (when down-sampling)
/// followed by linear interpolation. Plenty for speech recognition and cheap
/// enough to run inside the audio callback.
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    step: f64,
    /// Position of the next output sample, relative to `history[0]`.
    pos: f64,
    taps: Vec<f32>,
    /// Filter state: the last `taps.len() - 1` raw input samples.
    fir_state: Vec<f32>,
    /// Filtered samples not yet consumed by interpolation.
    history: Vec<f32>,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let taps = if in_rate > out_rate {
            lowpass_taps(31, 0.45 * out_rate as f64 / in_rate as f64)
        } else {
            vec![1.0]
        };
        Self {
            in_rate,
            out_rate,
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            fir_state: vec![0.0; taps.len() - 1],
            taps,
            history: Vec::new(),
        }
    }

    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.in_rate == self.out_rate {
            return input.to_vec();
        }
        // FIR low-pass over [state | input].
        let n = self.taps.len();
        let mut buf = Vec::with_capacity(self.fir_state.len() + input.len());
        buf.extend_from_slice(&self.fir_state);
        buf.extend_from_slice(input);
        for i in 0..input.len() {
            let window = &buf[i..i + n];
            let acc: f32 = window.iter().zip(&self.taps).map(|(x, t)| x * t).sum();
            self.history.push(acc);
        }
        let keep = buf.len().saturating_sub(n - 1);
        self.fir_state = buf[keep..].to_vec();

        // Linear interpolation at fractional positions.
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        while self.pos + 1.0 < self.history.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.history[i] * (1.0 - frac) + self.history[i + 1] * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(self.history.len());
        self.history.drain(..consumed);
        self.pos -= consumed as f64;
        out
    }
}

fn lowpass_taps(n: usize, cutoff: f64) -> Vec<f32> {
    // Hann-windowed sinc, normalized to unity DC gain. `cutoff` is a fraction of
    // the input sample rate.
    let m = (n - 1) as f64;
    let mut taps: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos();
            sinc * window
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// One-shot resampling of a whole PCM16 buffer.
pub fn resample_i16(samples: &[i16], in_rate: u32, out_rate: u32) -> Vec<i16> {
    if in_rate == out_rate {
        return samples.to_vec();
    }
    let mut r = Resampler::new(in_rate, out_rate);
    let mut out = r.process(&i16_to_f32(samples));
    // Flush the filter delay with silence so the tail is not cut.
    out.extend(r.process(&vec![0.0; 64]));
    let expected = (samples.len() as u64 * out_rate as u64 / in_rate as u64) as usize;
    out.truncate(expected);
    f32_to_i16(&out)
}

pub fn pcm_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Wrap mono PCM16 in a canonical 44-byte WAV header.
pub fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend(pcm_to_le_bytes(samples));
    out
}

/// Decoded mono PCM16 WAV.
#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

/// Parse a PCM16 WAV (mono or multi-channel, down-mixed). Used by the smoke
/// test and fixtures; the app never reads files on the dictation path.
pub fn parse_wav(bytes: &[u8]) -> Result<Wav, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("to nie jest plik WAV".into());
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + len).min(bytes.len());
        let body = &bytes[body_start..body_end];
        if id == b"fmt " && body.len() >= 16 {
            let format = u16::from_le_bytes([body[0], body[1]]);
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            fmt = Some((format, channels, rate, bits));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt.ok_or("brak bloku fmt przed danymi")?;
            if !(format == 1 || format == 0xFFFE) || bits != 16 {
                return Err(format!("obsługiwany jest tylko PCM16 (format {format}, {bits} bit)"));
            }
            let raw: Vec<i16> = body
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            let samples = if channels > 1 {
                f32_to_i16(&downmix(&i16_to_f32(&raw), channels as usize))
            } else {
                raw
            };
            return Ok(Wav { sample_rate: rate, samples });
        }
        pos = body_start + len + (len & 1);
    }
    Err("brak bloku data".into())
}

/// RMS level of a chunk in dBFS (−100 for silence).
pub fn rms_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt() / i16::MAX as f64;
    if rms <= 1e-5 {
        -100.0
    } else {
        (20.0 * rms.log10()) as f32
    }
}

/// Meter value 0..1 for the overlay (−60 dBFS → 0, −10 dBFS → 1).
pub fn meter_level(samples: &[i16]) -> f32 {
    ((rms_dbfs(samples) + 60.0) / 50.0).clamp(0.0, 1.0)
}

/// Conservative speech check: at least ~100 ms of 20 ms frames clearly above
/// both an absolute floor and the recording's own noise floor. Only used to
/// skip obviously empty recordings (accidental presses), never to trim.
pub fn contains_speech(samples: &[i16], sample_rate: u32) -> bool {
    let frame = (sample_rate as usize / 50).max(1);
    let mut levels: Vec<f32> = samples.chunks(frame).map(rms_dbfs).collect();
    if levels.is_empty() {
        return false;
    }
    let loud_needed = 5;
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise_floor = levels[levels.len() / 10];
    let threshold = (noise_floor + 10.0).max(-50.0);
    levels.iter().filter(|&&l| l > threshold).count() >= loud_needed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn resampler_preserves_duration_in_chunks() {
        for in_rate in [48_000u32, 44_100, 32_000, 16_000] {
            let input = sine(440.0, in_rate, 1.0, 0.5);
            let mut r = Resampler::new(in_rate, TARGET_RATE);
            let mut out = Vec::new();
            for chunk in input.chunks(480) {
                out.extend(r.process(chunk));
            }
            let diff = (out.len() as i64 - 16_000).abs();
            assert!(diff <= 2, "{in_rate}: got {} samples", out.len());
        }
    }

    #[test]
    fn resampler_keeps_speech_band_and_kills_aliasing() {
        // 1 kHz passes, 12 kHz (above the 8 kHz Nyquist of the output) is removed.
        let pass = Resampler::new(48_000, 16_000).process(&sine(1000.0, 48_000, 0.5, 0.5));
        let stop = Resampler::new(48_000, 16_000).process(&sine(12_000.0, 48_000, 0.5, 0.5));
        let pass_rms = rms(&pass[200..]);
        let stop_rms = rms(&stop[200..]);
        assert!((pass_rms - 0.5 / 2f32.sqrt()).abs() < 0.03, "pass {pass_rms}");
        assert!(stop_rms < 0.02, "stop {stop_rms}");
    }

    #[test]
    fn upsampling_for_openai_realtime() {
        let input: Vec<i16> = f32_to_i16(&sine(300.0, 16_000, 0.25, 0.5));
        let out = resample_i16(&input, 16_000, 24_000);
        assert_eq!(out.len(), 6_000);
        assert!(rms_dbfs(&out) > -10.0);
    }

    #[test]
    fn wav_roundtrip() {
        let samples: Vec<i16> = (0..1000).map(|i| (i * 7 % 2000 - 1000) as i16).collect();
        let bytes = wav_bytes(&samples, 16_000);
        assert_eq!(bytes.len(), 44 + 2000);
        assert_eq!(&bytes[0..4], b"RIFF");
        let wav = parse_wav(&bytes).unwrap();
        assert_eq!(wav, Wav { sample_rate: 16_000, samples });
    }

    #[test]
    fn parse_wav_rejects_non_pcm16() {
        let mut bytes = wav_bytes(&[0; 10], 16_000);
        bytes[34] = 32; // bits per sample
        assert!(parse_wav(&bytes).is_err());
        assert!(parse_wav(b"nope").is_err());
    }

    #[test]
    fn levels_and_speech_detection() {
        assert_eq!(rms_dbfs(&[0; 100]), -100.0);
        let tone = f32_to_i16(&sine(300.0, 16_000, 1.0, 0.3));
        assert!((rms_dbfs(&tone) - (-13.5)).abs() < 0.5);
        assert!(meter_level(&tone) > 0.8);

        let silence = vec![0i16; 16_000];
        assert!(!contains_speech(&silence, 16_000));
        let hiss: Vec<i16> = (0..16_000).map(|i| ((i * 7919) % 41) as i16 - 20).collect();
        assert!(!contains_speech(&hiss, 16_000));

        // 300 ms of "speech" inside 1 s of hiss.
        let mut mixed = hiss.clone();
        mixed[4_000..8_800].copy_from_slice(&tone[..4_800]);
        assert!(contains_speech(&mixed, 16_000));
    }
}

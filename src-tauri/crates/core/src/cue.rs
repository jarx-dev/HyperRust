//! Start/stop cues played around a recording. The tones are synthesized into
//! in-memory WAV files, so there are no sound assets to bundle and the app can
//! hand the bytes straight to the system player.

use std::f32::consts::TAU;

use crate::audio::wav_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Recording started: a short high pip.
    Start,
    /// Recording ended and goes to transcription: a short lower pip.
    Stop,
}

const RATE: u32 = 22_050;
/// Peak level relative to full scale; quiet enough not to startle.
const AMPLITUDE: f32 = 0.3;
/// Above 800 Hz, where small laptop speakers still play at full level
/// (a 45 ms pip at 700–1000 Hz and 0.22 was inaudible on the user's laptop).
const START_HZ: f32 = 1_320.0;
const STOP_HZ: f32 = 880.0;
/// Short enough to read as a pip rather than a tone.
const PIP_MS: u32 = 80;

/// The cue as a 16-bit mono PCM WAV file.
pub fn wav(cue: Cue) -> Vec<u8> {
    let hz = match cue {
        Cue::Start => START_HZ,
        Cue::Stop => STOP_HZ,
    };
    let samples: Vec<i16> = pip(hz, PIP_MS).collect();
    wav_bytes(&samples, RATE)
}

fn samples_for(ms: u32) -> usize {
    (RATE * ms / 1000) as usize
}

/// A sine pip with a short attack, an exponential decay and a linear release,
/// so it starts and ends at silence (no clicks).
fn pip(hz: f32, ms: u32) -> impl Iterator<Item = i16> {
    let n = samples_for(ms);
    let attack = samples_for(3) as f32;
    let release = samples_for(10) as f32;
    (0..n).map(move |i| {
        let x = i as f32;
        let env = (x / attack).min(1.0) * ((n - i) as f32 / release).min(1.0) * (-3.0 * x / n as f32).exp();
        let s = (TAU * hz * x / RATE as f32).sin() * env * AMPLITUDE;
        (s * i16::MAX as f32) as i16
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::parse_wav;

    #[test]
    fn cues_are_short_quiet_pips() {
        for cue in [Cue::Start, Cue::Stop] {
            let w = parse_wav(&wav(cue)).expect("valid wav");
            assert_eq!(w.sample_rate, RATE);
            let ms = w.samples.len() as u32 * 1000 / RATE;
            assert!((75..=85).contains(&ms), "{ms} ms");
            let peak = w.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 1000 && f32::from(peak) <= AMPLITUDE * f32::from(i16::MAX) + 1.0, "peak {peak}");
            assert_eq!(w.samples[0], 0);
            assert!(w.samples.last().unwrap().unsigned_abs() < 200);
        }
    }

    #[test]
    fn start_and_stop_differ() {
        assert_ne!(wav(Cue::Start), wav(Cue::Stop));
    }
}

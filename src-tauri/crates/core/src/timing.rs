//! Stage timings of one dictation, logged so latency can be measured on the
//! target machine. All offsets are relative to the key press.

use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone)]
pub struct Timeline {
    pub pressed: Instant,
    pub released: Option<Instant>,
    pub stt_connected: Option<Instant>,
    pub stt_done: Option<Instant>,
    pub llm_first_token: Option<Instant>,
    pub llm_done: Option<Instant>,
    pub pasted: Option<Instant>,
}

impl Timeline {
    pub fn start() -> Self {
        Self::starting_at(Instant::now())
    }

    pub fn starting_at(pressed: Instant) -> Self {
        Self {
            pressed,
            released: None,
            stt_connected: None,
            stt_done: None,
            llm_first_token: None,
            llm_done: None,
            pasted: None,
        }
    }

    pub fn report(&self, meta: TimingMeta) -> TimingReport {
        let rel = |t: Option<Instant>| t.map(|t| ms(t.saturating_duration_since(self.released.unwrap_or(self.pressed))));
        TimingReport {
            audio_ms: self.released.map(|r| ms(r - self.pressed)),
            stt_connect_ms: self.stt_connected.map(|t| ms(t.saturating_duration_since(self.pressed))),
            release_to_stt_ms: rel(self.stt_done),
            llm_ttft_ms: match (self.stt_done, self.llm_first_token) {
                (Some(a), Some(b)) => Some(ms(b.saturating_duration_since(a))),
                _ => None,
            },
            llm_ms: match (self.stt_done, self.llm_done) {
                (Some(a), Some(b)) => Some(ms(b.saturating_duration_since(a))),
                _ => None,
            },
            paste_ms: match (self.llm_done.or(self.stt_done), self.pasted) {
                (Some(a), Some(b)) => Some(ms(b.saturating_duration_since(a))),
                _ => None,
            },
            release_to_paste_ms: rel(self.pasted),
            meta,
        }
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TimingMeta {
    pub mode: String,
    pub stt: String,
    pub streamed: bool,
    pub llm: String,
    pub chars: usize,
    pub outcome: String,
    /// USD for the speech part: what the provider billed (OpenRouter), else
    /// the built-in price list times the recording (`stt_cost_estimated`).
    pub stt_cost_usd: Option<f64>,
    pub stt_cost_estimated: bool,
    /// USD the post-processing endpoint billed (OpenRouter); None on a subscription.
    pub llm_cost_usd: Option<f64>,
}

/// One row of the latency log (`release` = key release, the moment the user
/// starts waiting).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TimingReport {
    pub audio_ms: Option<u64>,
    pub stt_connect_ms: Option<u64>,
    pub release_to_stt_ms: Option<u64>,
    pub llm_ttft_ms: Option<u64>,
    pub llm_ms: Option<u64>,
    pub paste_ms: Option<u64>,
    pub release_to_paste_ms: Option<u64>,
    #[serde(flatten)]
    pub meta: TimingMeta,
}

impl std::fmt::Display for TimingReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = |x: Option<u64>| x.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        let usd = |x: Option<f64>| x.map(|n| format!("{n:.6}")).unwrap_or_else(|| "-".into());
        write!(
            f,
            "timing outcome={} mode={} stt={} streamed={} llm={} audio_ms={} stt_connect_ms={} release_to_stt_ms={} llm_ttft_ms={} llm_ms={} paste_ms={} release_to_paste_ms={} chars={} stt_cost_usd={}{} llm_cost_usd={}",
            self.meta.outcome,
            self.meta.mode,
            self.meta.stt,
            self.meta.streamed,
            if self.meta.llm.is_empty() { "-" } else { &self.meta.llm },
            v(self.audio_ms),
            v(self.stt_connect_ms),
            v(self.release_to_stt_ms),
            v(self.llm_ttft_ms),
            v(self.llm_ms),
            v(self.paste_ms),
            v(self.release_to_paste_ms),
            self.meta.chars,
            usd(self.meta.stt_cost_usd),
            if self.meta.stt_cost_estimated { "~" } else { "" },
            usd(self.meta.llm_cost_usd),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_measures_from_release() {
        let p = Instant::now();
        let at = |ms: u64| Some(p + Duration::from_millis(ms));
        let t = Timeline {
            pressed: p,
            released: at(3000),
            stt_connected: at(250),
            stt_done: at(3400),
            llm_first_token: at(3700),
            llm_done: at(4100),
            pasted: at(4150),
        };
        let r = t.report(TimingMeta { outcome: "ok".into(), ..Default::default() });
        assert_eq!(r.audio_ms, Some(3000));
        assert_eq!(r.stt_connect_ms, Some(250));
        assert_eq!(r.release_to_stt_ms, Some(400));
        assert_eq!(r.llm_ttft_ms, Some(300));
        assert_eq!(r.llm_ms, Some(700));
        assert_eq!(r.paste_ms, Some(50));
        assert_eq!(r.release_to_paste_ms, Some(1150));
        let line = r.to_string();
        assert!(line.contains("release_to_paste_ms=1150") && line.contains("llm=-"));
        assert!(line.ends_with("stt_cost_usd=- llm_cost_usd=-"));
        let priced = t.report(TimingMeta { stt_cost_usd: Some(0.0011), stt_cost_estimated: true, llm_cost_usd: Some(0.00004), ..Default::default() });
        assert!(priced.to_string().ends_with("stt_cost_usd=0.001100~ llm_cost_usd=0.000040"));
    }

    #[test]
    fn report_without_llm_measures_paste_from_stt() {
        let p = Instant::now();
        let t = Timeline {
            released: Some(p + Duration::from_millis(1000)),
            stt_done: Some(p + Duration::from_millis(1300)),
            pasted: Some(p + Duration::from_millis(1340)),
            ..Timeline::starting_at(p)
        };
        let r = t.report(TimingMeta::default());
        assert_eq!(r.llm_ms, None);
        assert_eq!(r.paste_ms, Some(40));
        assert_eq!(r.release_to_paste_ms, Some(340));
    }
}

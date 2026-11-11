//! Non-secret settings, persisted as JSON by the app. API keys never live here:
//! the app keeps them in the OS credential store (see [`SecretKey`]).

use serde::{Deserialize, Serialize};

use crate::dictionary::DictionaryEntry;
use crate::modes::{default_modes, Mode, AGENT_MODE_ID};
use crate::stt::SttProviderKind;

/// OpenAI-compatible endpoint a new installation post-processes with, baked in
/// at build time (`HYPERRUST_DEFAULT_LLM_URL`, e.g. a CLIProxy in your tailnet).
/// Empty = new installations use OpenRouter.
pub const DEFAULT_LLM_BASE_URL: &str = match option_env!("HYPERRUST_DEFAULT_LLM_URL") {
    Some(url) => url,
    None => "",
};
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    /// Hold to talk; a double tap latches hands-free recording.
    #[default]
    Auto,
    /// Recording lasts exactly as long as the key is held.
    Hold,
    /// First press starts, next press stops.
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    /// Any OpenAI-compatible `/chat/completions` endpoint, e.g. CLIProxy running
    /// on the user's subscriptions (no per-token cost).
    #[default]
    OpenAiCompatible,
    /// OpenRouter, reusing the OpenRouter key from the STT settings (paid per token).
    OpenRouter,
    /// `claude -p` from Claude Code (uses the user's subscription).
    ClaudeCli,
    /// `codex exec` from Codex CLI (uses the user's subscription).
    CodexCli,
}

/// Names of the secrets kept in the OS credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKey {
    ElevenLabs,
    /// Key of a second ElevenLabs account, used when the first one fails.
    ElevenLabsBackup,
    Xai,
    OpenAi,
    OpenRouter,
    /// Key for the OpenAI-compatible post-processing endpoint.
    LlmEndpoint,
}

impl SecretKey {
    pub const ALL: [SecretKey; 6] = [
        SecretKey::ElevenLabs,
        SecretKey::ElevenLabsBackup,
        SecretKey::Xai,
        SecretKey::OpenAi,
        SecretKey::OpenRouter,
        SecretKey::LlmEndpoint,
    ];

    /// Account name in the credential store.
    pub fn account(self) -> &'static str {
        match self {
            SecretKey::ElevenLabs => "elevenlabs",
            SecretKey::ElevenLabsBackup => "elevenlabs-backup",
            SecretKey::Xai => "xai",
            SecretKey::OpenAi => "openai",
            SecretKey::OpenRouter => "openrouter",
            SecretKey::LlmEndpoint => "llm-endpoint",
        }
    }

    /// Environment variable used by the headless smoke test.
    pub fn env_var(self) -> &'static str {
        match self {
            SecretKey::ElevenLabs => "ELEVENLABS_API_KEY",
            SecretKey::ElevenLabsBackup => "ELEVENLABS_BACKUP_API_KEY",
            SecretKey::Xai => "XAI_API_KEY",
            SecretKey::OpenAi => "OPENAI_API_KEY",
            SecretKey::OpenRouter => "OPENROUTER_API_KEY",
            SecretKey::LlmEndpoint => "CLIPROXY_API_KEY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttModels {
    pub elevenlabs: String,
    pub elevenlabs_realtime: String,
    pub xai: String,
    pub openai: String,
    pub openai_realtime: String,
    pub openrouter: String,
}

impl Default for SttModels {
    fn default() -> Self {
        Self {
            elevenlabs: "scribe_v2".into(),
            elevenlabs_realtime: "scribe_v2_realtime".into(),
            xai: "grok-voice-transcribe-2.0".into(),
            openai: "gpt-transcribe".into(),
            openai_realtime: "gpt-live-transcribe".into(),
            openrouter: "openai/gpt-transcribe".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttSettings {
    pub provider: SttProviderKind,
    /// ISO-639-1 language code; empty means auto-detect.
    pub language: String,
    /// Stream audio while the key is held (when the provider supports it).
    pub streaming: bool,
    pub models: SttModels,
    /// How long to wait for the final transcript after the key is released.
    pub final_timeout_ms: u64,
    /// Names the speech model gets wrong; see [`crate::dictionary`].
    pub dictionary: Vec<DictionaryEntry>,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            provider: SttProviderKind::ElevenLabs,
            language: "pl".into(),
            streaming: true,
            models: SttModels::default(),
            final_timeout_ms: 10_000,
            dictionary: crate::dictionary::default_entries(),
        }
    }
}

impl SttSettings {
    pub fn language(&self) -> Option<&str> {
        let l = self.language.trim();
        (!l.is_empty()).then_some(l)
    }

    pub fn batch_model(&self) -> &str {
        match self.provider {
            SttProviderKind::ElevenLabs => &self.models.elevenlabs,
            SttProviderKind::Xai => &self.models.xai,
            SttProviderKind::OpenAi => &self.models.openai,
            SttProviderKind::OpenRouter => &self.models.openrouter,
        }
    }

    pub fn realtime_model(&self) -> &str {
        match self.provider {
            SttProviderKind::ElevenLabs => &self.models.elevenlabs_realtime,
            SttProviderKind::Xai => &self.models.xai,
            SttProviderKind::OpenAi => &self.models.openai_realtime,
            SttProviderKind::OpenRouter => &self.models.openrouter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmSettings {
    pub backend: LlmBackend,
    pub base_url: String,
    pub model: String,
    pub openrouter_model: String,
    /// Optional `--model` for the CLI backends; empty = CLI default.
    pub cli_model: String,
    /// Optional `reasoning_effort` sent to OpenAI-compatible endpoints; empty = omit.
    /// Keep it empty for Haiku: `none` makes it echo the transcript instead of editing it.
    /// The built-in cleanup prompt still runs Haiku with `none` (`llm::effective_effort`).
    pub reasoning_effort: String,
    pub timeout_ms: u64,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            backend: if DEFAULT_LLM_BASE_URL.is_empty() { LlmBackend::OpenRouter } else { LlmBackend::OpenAiCompatible },
            base_url: DEFAULT_LLM_BASE_URL.into(),
            // The newest fast Claude: the user wants only the latest models.
            model: "claude-haiku-5-5".into(),
            openrouter_model: "anthropic/claude-haiku-5.5".into(),
            cli_model: String::new(),
            reasoning_effort: String::new(),
            timeout_ms: 20_000,
        }
    }
}

impl LlmSettings {
    /// These settings with `mode`'s own model, when it has one, on the active backend.
    pub fn for_mode(&self, mode: &Mode) -> LlmSettings {
        let mut s = self.clone();
        let model = mode.llm_model.trim();
        if !model.is_empty() {
            match s.backend {
                LlmBackend::OpenAiCompatible => s.model = model.into(),
                LlmBackend::OpenRouter => s.openrouter_model = model.into(),
                LlmBackend::ClaudeCli | LlmBackend::CodexCli => s.cli_model = model.into(),
            }
        }
        s
    }

    /// Base URL and model for the HTTP backends.
    pub fn http_target(&self) -> Option<(&str, &str)> {
        match self.backend {
            LlmBackend::OpenAiCompatible => Some((self.base_url.trim(), self.model.trim())),
            LlmBackend::OpenRouter => Some((OPENROUTER_BASE_URL, self.openrouter_model.trim())),
            LlmBackend::ClaudeCli | LlmBackend::CodexCli => None,
        }
    }

    pub fn secret(&self) -> Option<SecretKey> {
        match self.backend {
            LlmBackend::OpenAiCompatible => Some(SecretKey::LlmEndpoint),
            LlmBackend::OpenRouter => Some(SecretKey::OpenRouter),
            LlmBackend::ClaudeCli | LlmBackend::CodexCli => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Keep the microphone stream open between dictations (no device start-up
    /// cost, pre-roll before the key press). Windows shows the mic-in-use icon.
    pub keep_warm: bool,
    /// Input device name; empty = system default.
    pub device: String,
    /// Audio kept from before the key press (only with `keep_warm`).
    pub preroll_ms: u32,
    /// Recordings shorter than this are discarded as accidental presses.
    pub min_duration_ms: u32,
    /// Short tone when a recording starts and when it ends.
    pub cues: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            keep_warm: true,
            device: String::new(),
            preroll_ms: 300,
            min_duration_ms: 250,
            cues: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteSettings {
    /// Paste into the active window; when false the text only lands in the clipboard.
    pub auto_paste: bool,
    /// Restore the previous clipboard content after pasting.
    pub restore_clipboard: bool,
    pub restore_delay_ms: u64,
}

impl Default for PasteSettings {
    fn default() -> Self {
        Self {
            auto_paste: true,
            restore_clipboard: true,
            restore_delay_ms: 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Poll the update server and install newer builds when idle.
    pub enabled: bool,
    /// URL of `latest.json` (see [`crate::update::validate_url`]).
    pub url: String,
    pub interval_s: u64,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            url: crate::update::DEFAULT_UPDATE_URL.into(),
            interval_s: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    /// Keep the raw transcript and the pasted text of recent dictations.
    pub enabled: bool,
    /// History server that receives the entries; empty = keep them only locally.
    pub upload_url: String,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            upload_url: crate::history::DEFAULT_HISTORY_URL.into(),
        }
    }
}

impl HistorySettings {
    /// Upload target, when uploads are on.
    pub fn upload_target(&self) -> Option<&str> {
        let url = self.upload_url.trim();
        (self.enabled && !url.is_empty()).then_some(url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: String,
    pub hotkey_mode: HotkeyMode,
    pub active_mode: String,
    pub modes: Vec<Mode>,
    pub stt: SttSettings,
    pub llm: LlmSettings,
    pub audio: AudioSettings,
    pub paste: PasteSettings,
    pub updates: UpdateSettings,
    pub history: HistorySettings,
    /// Periodically touch the configured endpoints so connections stay warm.
    pub keep_connections_warm: bool,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Shift+Space".into(),
            hotkey_mode: HotkeyMode::Auto,
            active_mode: AGENT_MODE_ID.into(),
            modes: default_modes(),
            stt: SttSettings::default(),
            llm: LlmSettings::default(),
            audio: AudioSettings::default(),
            paste: PasteSettings::default(),
            updates: UpdateSettings::default(),
            history: HistorySettings::default(),
            keep_connections_warm: true,
            autostart: false,
        }
    }
}

impl Settings {
    /// Parse persisted JSON; unknown or missing fields fall back to defaults and
    /// the result is normalized, so a damaged file never blocks start-up.
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str::<Settings>(json)
            .unwrap_or_default()
            .normalized()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("settings serialize")
    }

    /// Repair invariants: at least one mode, unique mode ids, an existing active mode.
    pub fn normalized(mut self) -> Self {
        let mut seen = std::collections::HashSet::new();
        self.modes.retain(|m| !m.id.trim().is_empty() && seen.insert(m.id.clone()));
        if self.modes.is_empty() {
            self.modes = default_modes();
        }
        // Make the preset implied by older settings explicit.
        for m in &mut self.modes {
            m.preset = Some(m.preset());
        }
        if !self.modes.iter().any(|m| m.id == self.active_mode) {
            self.active_mode = self.modes[0].id.clone();
        }
        if self.hotkey.trim().is_empty() || crate::hotkey::HotkeySpec::parse(&self.hotkey).is_err() {
            self.hotkey = Settings::default().hotkey;
        }
        self.updates.interval_s = self.updates.interval_s.clamp(15, 86_400);
        self.stt.dictionary = crate::dictionary::normalized(std::mem::take(&mut self.stt.dictionary));
        self
    }

    pub fn active_mode(&self) -> &Mode {
        self.modes
            .iter()
            .find(|m| m.id == self.active_mode)
            .unwrap_or(&self.modes[0])
    }

    /// The mode after the active one, wrapping around; `None` with a single mode.
    pub fn next_mode(&self) -> Option<&Mode> {
        if self.modes.len() < 2 {
            return None;
        }
        let i = self.modes.iter().position(|m| m.id == self.active_mode).unwrap_or(0);
        self.modes.get((i + 1) % self.modes.len())
    }

    /// Whether any mode sends the transcript to the post-processing backend.
    pub fn post_processes(&self) -> bool {
        self.modes.iter().any(|m| m.post_process)
    }

    /// Speech goes to ElevenLabs, so its balance matters (overview, tray).
    pub fn uses_elevenlabs(&self) -> bool {
        self.stt.provider == SttProviderKind::ElevenLabs
    }

    /// Speech or post-processing goes to OpenRouter.
    pub fn uses_openrouter(&self) -> bool {
        self.stt.provider == SttProviderKind::OpenRouter
            || (self.llm.backend == LlmBackend::OpenRouter && self.post_processes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::RAW_MODE_ID;

    #[test]
    fn next_mode_cycles_through_all_modes() {
        let mut s = Settings::default();
        assert_eq!(s.next_mode().map(|m| m.id.as_str()), Some(RAW_MODE_ID));
        s.active_mode = RAW_MODE_ID.into();
        assert_eq!(s.next_mode().map(|m| m.id.as_str()), Some(AGENT_MODE_ID));
        s.active_mode = "nie-ma".into(); // acts as the first mode
        assert_eq!(s.next_mode().map(|m| m.id.as_str()), Some(RAW_MODE_ID));
        s.modes.truncate(1);
        assert!(s.next_mode().is_none());
    }

    #[test]
    fn balances_follow_the_providers_in_use() {
        let mut s = Settings::default();
        s.llm.backend = LlmBackend::OpenAiCompatible;
        assert!(s.uses_elevenlabs());
        assert!(!s.uses_openrouter());
        s.stt.provider = SttProviderKind::OpenRouter;
        assert!(!s.uses_elevenlabs());
        assert!(s.uses_openrouter());
        s.stt.provider = SttProviderKind::Xai;
        s.llm.backend = LlmBackend::OpenRouter;
        assert!(s.uses_openrouter());
        // No mode post-processes: the backend is never called.
        s.modes.iter_mut().for_each(|m| m.post_process = false);
        assert!(!s.uses_openrouter());
    }

    #[test]
    fn default_roundtrips_through_json() {
        let s = Settings::default();
        assert_eq!(Settings::from_json(&s.to_json()), s);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s = Settings::from_json(r#"{"hotkey":"F9","stt":{"provider":"xai"}}"#);
        assert_eq!(s.hotkey, "F9");
        assert_eq!(s.stt.provider, SttProviderKind::Xai);
        assert_eq!(s.stt.language, "pl");
        assert_eq!(s.llm, LlmSettings::default());
        assert_eq!(s.active_mode().id, AGENT_MODE_ID);
    }

    #[test]
    fn garbage_json_yields_defaults() {
        assert_eq!(Settings::from_json("{not json"), Settings::default());
    }

    #[test]
    fn normalization_repairs_modes_and_hotkey() {
        let mut s = Settings::default();
        s.modes.push(s.modes[0].clone()); // duplicate id
        s.active_mode = "nie-ma".into();
        s.hotkey = "Ctrl+Bzdura".into();
        let n = s.normalized();
        assert_eq!(n.modes.len(), 2);
        assert_eq!(n.active_mode, AGENT_MODE_ID);
        assert_eq!(n.hotkey, Settings::default().hotkey);

        let fast = Settings { updates: UpdateSettings { interval_s: 1, ..UpdateSettings::default() }, ..Settings::default() };
        assert_eq!(fast.normalized().updates.interval_s, 15);

        let empty = Settings { modes: vec![], ..Settings::default() }.normalized();
        assert_eq!(empty.modes, default_modes());
    }

    #[test]
    fn old_modes_get_an_explicit_preset_and_modes_can_pick_a_model() {
        let s = Settings::from_json(
            r#"{"modes":[{"id":"prompt","name":"P","post_process":true},{"id":"raw","name":"R","post_process":true}]}"#,
        );
        assert_eq!(s.modes[0].preset, Some(crate::modes::Preset::Agent));
        assert_eq!(s.modes[1].preset, Some(crate::modes::Preset::Cleanup));

        let mut mode = s.modes[0].clone();
        let llm = LlmSettings { backend: LlmBackend::OpenAiCompatible, ..LlmSettings::default() };
        assert_eq!(llm.for_mode(&mode), llm);
        mode.llm_model = " claude-haiku-4-5 ".into();
        assert_eq!(llm.for_mode(&mode).model, "claude-haiku-4-5");
        let router = LlmSettings { backend: LlmBackend::OpenRouter, ..LlmSettings::default() };
        assert_eq!(router.for_mode(&mode).openrouter_model, "claude-haiku-4-5");
        assert_eq!(router.for_mode(&mode).model, router.model);
    }

    #[test]
    fn llm_targets_and_secrets() {
        let mut l = LlmSettings { backend: LlmBackend::OpenAiCompatible, ..LlmSettings::default() };
        assert_eq!(l.http_target(), Some((DEFAULT_LLM_BASE_URL, "claude-haiku-5-5")));
        assert_eq!(l.secret(), Some(SecretKey::LlmEndpoint));
        l.backend = LlmBackend::OpenRouter;
        assert_eq!(l.http_target(), Some((OPENROUTER_BASE_URL, "anthropic/claude-haiku-5.5")));
        assert_eq!(l.secret(), Some(SecretKey::OpenRouter));
        l.backend = LlmBackend::ClaudeCli;
        assert_eq!(l.http_target(), None);
        assert_eq!(l.secret(), None);
    }

    #[test]
    fn empty_language_means_autodetect() {
        let mut s = SttSettings::default();
        assert_eq!(s.language(), Some("pl"));
        s.language = " ".into();
        assert_eq!(s.language(), None);
    }

    #[test]
    fn secret_accounts_are_unique() {
        let set: std::collections::HashSet<_> = SecretKey::ALL.iter().map(|k| k.account()).collect();
        assert_eq!(set.len(), SecretKey::ALL.len());
    }
}

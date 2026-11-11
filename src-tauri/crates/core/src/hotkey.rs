//! Global hotkey model: parsing the shortcut, matching raw key events from a
//! low-level keyboard hook, and the push-to-talk state machine.
//!
//! The Windows hook in the app crate only forwards `(virtual-key, down/up)`
//! pairs; every decision lives here so it is unit-tested without Windows.
//! Behaviour: hold to talk, a quick double tap latches hands-free recording,
//! another key pressed while a bare modifier shortcut is held means it was a
//! keyboard shortcut, not dictation.

use std::fmt;

use crate::config::HotkeyMode;

/// Windows virtual-key codes used by the matcher.
pub mod vk {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const RETURN: u32 = 0x0D;
    pub const SHIFT: u32 = 0x10;
    pub const CONTROL: u32 = 0x11;
    pub const MENU: u32 = 0x12;
    pub const PAUSE: u32 = 0x13;
    pub const CAPITAL: u32 = 0x14;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const INSERT: u32 = 0x2D;
    pub const DELETE: u32 = 0x2E;
    pub const LWIN: u32 = 0x5B;
    pub const RWIN: u32 = 0x5C;
    pub const APPS: u32 = 0x5D;
    pub const NUMPAD0: u32 = 0x60;
    pub const F1: u32 = 0x70;
    pub const SCROLL: u32 = 0x91;
    pub const LSHIFT: u32 = 0xA0;
    pub const RSHIFT: u32 = 0xA1;
    pub const LCONTROL: u32 = 0xA2;
    pub const RCONTROL: u32 = 0xA3;
    pub const LMENU: u32 = 0xA4;
    pub const RMENU: u32 = 0xA5;
    pub const OEM_3: u32 = 0xC0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}

impl Mods {
    fn any(self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A non-modifier key, optionally with exactly these modifiers held.
    Combo { mods: Mods, key: u32 },
    /// A single, side-specific modifier key on its own (e.g. right Ctrl).
    ModifierOnly(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub trigger: Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyParseError {
    Empty,
    UnknownKey(String),
    TwoKeys,
    BareModifier,
    ModifierWithModifiers,
    Escape,
}

impl std::fmt::Display for HotkeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::tr;
        f.write_str(&match self {
            Self::Empty => tr!("empty shortcut", "pusty skrót"),
            Self::UnknownKey(key) => tr!("unknown key: {key}", "nieznany klawisz: {key}"),
            Self::TwoKeys => tr!("a shortcut can have only one main key", "skrót może mieć tylko jeden klawisz główny"),
            Self::BareModifier => tr!(
                "a modifier without a side (e.g. Ctrl) is unsafe; use e.g. RightCtrl",
                "sam modyfikator bez strony (np. Ctrl) jest niebezpieczny; użyj np. RightCtrl"
            ),
            Self::ModifierWithModifiers => {
                tr!("a modifier key (e.g. RightCtrl) must be used alone", "klawisz modyfikatora (np. RightCtrl) musi być użyty sam")
            }
            Self::Escape => tr!("Escape is reserved for cancelling a recording", "Escape jest zarezerwowany do anulowania nagrania"),
        })
    }
}

impl std::error::Error for HotkeyParseError {}

const SIDED_MODIFIERS: [(&str, u32); 8] = [
    ("rightctrl", vk::RCONTROL),
    ("leftctrl", vk::LCONTROL),
    ("rightshift", vk::RSHIFT),
    ("leftshift", vk::LSHIFT),
    ("rightalt", vk::RMENU),
    ("leftalt", vk::LMENU),
    ("rightwin", vk::RWIN),
    ("leftwin", vk::LWIN),
];

const NAMED_KEYS: [(&str, u32); 20] = [
    ("space", vk::SPACE),
    ("tab", vk::TAB),
    ("enter", vk::RETURN),
    ("backspace", vk::BACK),
    ("capslock", vk::CAPITAL),
    ("pause", vk::PAUSE),
    ("scrolllock", vk::SCROLL),
    ("insert", vk::INSERT),
    ("delete", vk::DELETE),
    ("home", vk::HOME),
    ("end", vk::END),
    ("pageup", vk::PRIOR),
    ("pagedown", vk::NEXT),
    ("left", vk::LEFT),
    ("up", vk::UP),
    ("right", vk::RIGHT),
    ("down", vk::DOWN),
    ("menu", vk::APPS),
    ("backquote", vk::OEM_3),
    ("escape", vk::ESCAPE),
];

fn key_from_name(lower: &str) -> Option<u32> {
    if let Some(&(_, code)) = NAMED_KEYS.iter().find(|(n, _)| *n == lower) {
        return Some(code);
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| vk::F1 + n - 1);
    }
    if let Some(n) = lower.strip_prefix("numpad").and_then(|d| d.parse::<u32>().ok()) {
        return (n <= 9).then(|| vk::NUMPAD0 + n);
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_lowercase() => Some(c.to_ascii_uppercase() as u32),
        (Some(c), None) if c.is_ascii_digit() => Some(c as u32),
        _ => None,
    }
}

fn key_name(code: u32) -> String {
    if let Some(&(n, _)) = NAMED_KEYS.iter().find(|(_, c)| *c == code) {
        return capitalize(n);
    }
    if let Some(&(n, _)) = SIDED_MODIFIERS.iter().find(|(_, c)| *c == code) {
        return match n {
            "rightctrl" => "RightCtrl",
            "leftctrl" => "LeftCtrl",
            "rightshift" => "RightShift",
            "leftshift" => "LeftShift",
            "rightalt" => "RightAlt",
            "leftalt" => "LeftAlt",
            "rightwin" => "RightWin",
            _ => "LeftWin",
        }
        .to_string();
    }
    match code {
        c if (vk::F1..vk::F1 + 24).contains(&c) => format!("F{}", c - vk::F1 + 1),
        c if (vk::NUMPAD0..=vk::NUMPAD0 + 9).contains(&c) => format!("Numpad{}", c - vk::NUMPAD0),
        c if (0x30..=0x39).contains(&c) || (0x41..=0x5A).contains(&c) => {
            char::from_u32(c).map(String::from).unwrap_or_default()
        }
        c => format!("VK{c:#04X}"),
    }
}

fn capitalize(s: &str) -> String {
    match s {
        "capslock" => "CapsLock".into(),
        "scrolllock" => "ScrollLock".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "backquote" => "Backquote".into(),
        "backspace" => "Backspace".into(),
        _ => {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        }
    }
}

impl HotkeySpec {
    pub fn parse(s: &str) -> Result<Self, HotkeyParseError> {
        let tokens: Vec<String> = s
            .split('+')
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return Err(HotkeyParseError::Empty);
        }
        let mut mods = Mods::default();
        let mut key: Option<u32> = None;
        let mut sided: Option<u32> = None;
        for t in &tokens {
            match t.as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" => mods.alt = true,
                "win" | "super" | "meta" => mods.win = true,
                other => {
                    if let Some(&(_, code)) = SIDED_MODIFIERS.iter().find(|(n, _)| *n == other) {
                        if sided.replace(code).is_some() {
                            return Err(HotkeyParseError::TwoKeys);
                        }
                        continue;
                    }
                    let code =
                        key_from_name(other).ok_or_else(|| HotkeyParseError::UnknownKey(other.to_string()))?;
                    if code == vk::ESCAPE {
                        return Err(HotkeyParseError::Escape);
                    }
                    if key.replace(code).is_some() {
                        return Err(HotkeyParseError::TwoKeys);
                    }
                }
            }
        }
        let trigger = match (key, sided) {
            (Some(_), Some(_)) => return Err(HotkeyParseError::TwoKeys),
            (None, Some(code)) if mods.any() => {
                let _ = code;
                return Err(HotkeyParseError::ModifierWithModifiers);
            }
            (None, Some(code)) => Trigger::ModifierOnly(code),
            (Some(code), None) => Trigger::Combo { mods, key: code },
            (None, None) => return Err(HotkeyParseError::BareModifier),
        };
        Ok(HotkeySpec { trigger })
    }
}

impl fmt::Display for HotkeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.trigger {
            Trigger::ModifierOnly(code) => f.write_str(&key_name(code)),
            Trigger::Combo { mods, key } => {
                let mut parts: Vec<String> = Vec::new();
                if mods.ctrl {
                    parts.push("Ctrl".into());
                }
                if mods.shift {
                    parts.push("Shift".into());
                }
                if mods.alt {
                    parts.push("Alt".into());
                }
                if mods.win {
                    parts.push("Win".into());
                }
                parts.push(key_name(key));
                f.write_str(&parts.join("+"))
            }
        }
    }
}

fn is_modifier(code: u32) -> bool {
    matches!(
        code,
        vk::SHIFT
            | vk::CONTROL
            | vk::MENU
            | vk::LSHIFT
            | vk::RSHIFT
            | vk::LCONTROL
            | vk::RCONTROL
            | vk::LMENU
            | vk::RMENU
            | vk::LWIN
            | vk::RWIN
    )
}

/// What the hook observed, after matching against the configured shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Pressed,
    Released,
    /// Another key went down while a bare-modifier shortcut was held.
    Interference,
    /// Escape while a recording is active (cancel).
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    pub event: Option<KeyEvent>,
    /// Swallow the key so the focused app never sees it.
    pub swallow: bool,
}

/// Turns raw key transitions into shortcut events. Owned by the hook thread.
pub struct KeyMatcher {
    spec: HotkeySpec,
    held: [bool; 256],
    active: bool,
    interfered: bool,
    swallowing_escape: bool,
}

impl KeyMatcher {
    pub fn new(spec: HotkeySpec) -> Self {
        Self {
            spec,
            held: [false; 256],
            active: false,
            interfered: false,
            swallowing_escape: false,
        }
    }

    pub fn spec(&self) -> HotkeySpec {
        self.spec
    }

    /// Replace the shortcut; any in-progress press is forgotten.
    pub fn set_spec(&mut self, spec: HotkeySpec) {
        self.spec = spec;
        self.active = false;
        self.interfered = false;
    }

    fn held(&self, code: u32) -> bool {
        self.held.get(code as usize).copied().unwrap_or(false)
    }

    fn current_mods(&self) -> Mods {
        Mods {
            ctrl: self.held(vk::LCONTROL) || self.held(vk::RCONTROL) || self.held(vk::CONTROL),
            shift: self.held(vk::LSHIFT) || self.held(vk::RSHIFT) || self.held(vk::SHIFT),
            alt: self.held(vk::LMENU) || self.held(vk::RMENU) || self.held(vk::MENU),
            win: self.held(vk::LWIN) || self.held(vk::RWIN),
        }
    }

    /// Re-check tracked key state against a physical probe (`GetAsyncKeyState`):
    /// every modifier, plus every other key believed held. A key-up lost while
    /// the workstation was locked (Win+L) would otherwise block a bare-modifier
    /// shortcut or leave a modifier stuck. `current` is skipped: inside a
    /// low-level hook its physical state is not updated yet.
    pub fn resync(&mut self, current: u32, is_down: impl Fn(u32) -> bool) {
        for (code, held) in self.held.iter_mut().enumerate() {
            let code = code as u32;
            if code != current && (*held || is_modifier(code)) {
                *held = is_down(code);
            }
        }
    }

    /// Forget all transient state, e.g. when the hook resumes after the
    /// settings window captured a shortcut (key-ups during the pause were lost).
    pub fn reset(&mut self) {
        self.held = [false; 256];
        self.active = false;
        self.interfered = false;
        self.swallowing_escape = false;
    }

    /// Feed one key transition. `recording` enables Escape-to-cancel.
    pub fn on_key(&mut self, code: u32, down: bool, recording: bool) -> Decision {
        let idx = (code as usize).min(255);
        let repeat = down && self.held[idx];
        self.held[idx] = down;

        if code == vk::ESCAPE {
            if down && !repeat && recording {
                self.swallowing_escape = true;
                return Decision { event: Some(KeyEvent::Escape), swallow: true };
            }
            if self.swallowing_escape {
                if !down {
                    self.swallowing_escape = false;
                }
                return Decision { event: None, swallow: true };
            }
        }

        match self.spec.trigger {
            Trigger::ModifierOnly(trigger) => {
                if code == trigger {
                    if down && !repeat && !self.active {
                        let others_held = self
                            .held
                            .iter()
                            .enumerate()
                            .any(|(c, &h)| h && c as u32 != trigger);
                        if !others_held {
                            self.active = true;
                            self.interfered = false;
                            return Decision { event: Some(KeyEvent::Pressed), swallow: false };
                        }
                    } else if !down && self.active {
                        self.active = false;
                        return Decision { event: Some(KeyEvent::Released), swallow: false };
                    }
                } else if down && !repeat && self.active && !self.interfered {
                    self.interfered = true;
                    return Decision { event: Some(KeyEvent::Interference), swallow: false };
                }
                Decision::default()
            }
            Trigger::Combo { mods, key } => {
                if code != key {
                    return Decision::default();
                }
                if down {
                    if self.active {
                        return Decision { event: None, swallow: true };
                    }
                    if self.current_mods() == mods {
                        self.active = true;
                        return Decision { event: Some(KeyEvent::Pressed), swallow: true };
                    }
                    Decision::default()
                } else if self.active {
                    self.active = false;
                    Decision { event: Some(KeyEvent::Released), swallow: true }
                } else {
                    Decision::default()
                }
            }
        }
    }
}

/// What the dictation controller must do with the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttAction {
    Start,
    /// Stop and transcribe.
    Stop,
    /// Stop and throw the audio away.
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PttStep {
    pub action: Option<PttAction>,
    /// Call [`PttMachine::timeout`] at this time (ms) unless another event comes first.
    pub deadline: Option<u64>,
}

impl PttStep {
    fn act(a: PttAction) -> Self {
        PttStep { action: Some(a), deadline: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PttTiming {
    /// A press held at least this long is push-to-talk; shorter is a tap.
    pub hold_threshold_ms: u64,
    /// After a quick tap, a second press within this window latches recording.
    pub double_tap_window_ms: u64,
    /// Ignore stop presses this soon after latching (key bounce).
    pub min_latch_ms: u64,
}

impl Default for PttTiming {
    fn default() -> Self {
        // The double-tap window counts from the release, so it can stay short
        // and an accidental press is discarded quickly.
        Self { hold_threshold_ms: 250, double_tap_window_ms: 450, min_latch_ms: 300 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttState {
    Idle,
    Holding { since: u64 },
    TapPending { until: u64 },
    Latched { since: u64 },
}

/// Push-to-talk state machine. Time is passed in (ms) so it tests without sleeping.
#[derive(Debug, Clone)]
pub struct PttMachine {
    mode: HotkeyMode,
    timing: PttTiming,
    state: PttState,
    key_down: bool,
}

impl PttMachine {
    pub fn new(mode: HotkeyMode, timing: PttTiming) -> Self {
        Self { mode, timing, state: PttState::Idle, key_down: false }
    }

    pub fn set_mode(&mut self, mode: HotkeyMode) {
        self.mode = mode;
        self.reset();
    }

    /// Recording is running (or held for a possible double tap).
    pub fn is_recording(&self) -> bool {
        !matches!(self.state, PttState::Idle)
    }

    /// Forget everything, e.g. after the pipeline stopped the recording itself
    /// or the shortcut changed (its pending release will never be reported).
    pub fn reset(&mut self) {
        self.state = PttState::Idle;
        self.key_down = false;
    }

    pub fn key_down(&mut self, now: u64) -> PttStep {
        let was_down = std::mem::replace(&mut self.key_down, true);
        if was_down {
            return PttStep::default();
        }
        match (self.mode, self.state) {
            (HotkeyMode::Toggle, PttState::Idle) => {
                self.state = PttState::Latched { since: now };
                PttStep::act(PttAction::Start)
            }
            (_, PttState::Idle) => {
                self.state = PttState::Holding { since: now };
                PttStep::act(PttAction::Start)
            }
            (_, PttState::TapPending { until }) if now <= until => {
                self.state = PttState::Latched { since: now };
                PttStep::default()
            }
            (_, PttState::TapPending { .. }) => {
                // The window already elapsed but the timer has not fired yet:
                // treat it as a fresh press after discarding the stale tap.
                self.state = PttState::Holding { since: now };
                PttStep::act(PttAction::Start)
            }
            (_, PttState::Latched { since }) => {
                if now.saturating_sub(since) >= self.timing.min_latch_ms {
                    self.state = PttState::Idle;
                    PttStep::act(PttAction::Stop)
                } else {
                    PttStep::default()
                }
            }
            (_, PttState::Holding { .. }) => PttStep::default(),
        }
    }

    pub fn key_up(&mut self, now: u64) -> PttStep {
        self.key_down = false;
        match (self.mode, self.state) {
            (HotkeyMode::Hold, PttState::Holding { .. }) => {
                self.state = PttState::Idle;
                PttStep::act(PttAction::Stop)
            }
            (HotkeyMode::Auto, PttState::Holding { since }) => {
                if now.saturating_sub(since) >= self.timing.hold_threshold_ms {
                    self.state = PttState::Idle;
                    PttStep::act(PttAction::Stop)
                } else {
                    let until = now + self.timing.double_tap_window_ms;
                    self.state = PttState::TapPending { until };
                    PttStep { action: None, deadline: Some(until) }
                }
            }
            _ => PttStep::default(),
        }
    }

    pub fn timeout(&mut self, now: u64) -> PttStep {
        match self.state {
            PttState::TapPending { until } if now >= until => {
                self.state = PttState::Idle;
                PttStep::act(PttAction::Discard)
            }
            PttState::TapPending { until } => PttStep { action: None, deadline: Some(until) },
            _ => PttStep::default(),
        }
    }

    /// Another key was pressed while a bare-modifier shortcut was held.
    pub fn interference(&mut self) -> PttStep {
        match self.state {
            PttState::Holding { .. } | PttState::TapPending { .. } => {
                self.state = PttState::Idle;
                PttStep::act(PttAction::Discard)
            }
            PttState::Latched { .. } if self.key_down => {
                self.state = PttState::Idle;
                PttStep::act(PttAction::Discard)
            }
            _ => PttStep::default(),
        }
    }

    /// Escape or an external cancel.
    pub fn cancel(&mut self) -> PttStep {
        if self.is_recording() {
            self.state = PttState::Idle;
            PttStep::act(PttAction::Discard)
        } else {
            PttStep::default()
        }
    }

    /// The user clicked the recording indicator: stop and transcribe in any
    /// mode. A shortcut still held is released later without effect.
    pub fn finish(&mut self) -> PttStep {
        if self.is_recording() {
            self.state = PttState::Idle;
            PttStep::act(PttAction::Stop)
        } else {
            PttStep::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(s: &str) -> (Mods, u32) {
        match HotkeySpec::parse(s).unwrap().trigger {
            Trigger::Combo { mods, key } => (mods, key),
            t => panic!("not a combo: {t:?}"),
        }
    }

    #[test]
    fn parses_and_formats_shortcuts() {
        let (m, k) = combo("ctrl + shift + SPACE");
        assert!(m.ctrl && m.shift && !m.alt && !m.win);
        assert_eq!(k, vk::SPACE);
        for s in ["Ctrl+Shift+Space", "F9", "Alt+Win+K", "Ctrl+Numpad5", "Shift+F24", "Ctrl+Backquote"] {
            assert_eq!(HotkeySpec::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(HotkeySpec::parse("rightctrl").unwrap().to_string(), "RightCtrl");
        assert_eq!(
            HotkeySpec::parse("RightCtrl").unwrap().trigger,
            Trigger::ModifierOnly(vk::RCONTROL)
        );
    }

    #[test]
    fn rejects_bad_shortcuts() {
        assert_eq!(HotkeySpec::parse(""), Err(HotkeyParseError::Empty));
        assert_eq!(HotkeySpec::parse("Ctrl"), Err(HotkeyParseError::BareModifier));
        assert_eq!(HotkeySpec::parse("Ctrl+RightShift"), Err(HotkeyParseError::ModifierWithModifiers));
        assert_eq!(HotkeySpec::parse("A+B"), Err(HotkeyParseError::TwoKeys));
        assert_eq!(HotkeySpec::parse("F25"), Err(HotkeyParseError::UnknownKey("f25".into())));
        assert_eq!(HotkeySpec::parse("Ctrl+Escape"), Err(HotkeyParseError::Escape));
    }

    fn matcher(s: &str) -> KeyMatcher {
        KeyMatcher::new(HotkeySpec::parse(s).unwrap())
    }

    #[test]
    fn combo_requires_exact_modifiers_and_swallows_main_key() {
        let mut m = matcher("Ctrl+Shift+Space");
        assert_eq!(m.on_key(vk::LCONTROL, true, false), Decision::default());
        // Space with only Ctrl: not ours, passes through.
        assert_eq!(m.on_key(vk::SPACE, true, false), Decision::default());
        m.on_key(vk::SPACE, false, false);
        m.on_key(vk::RSHIFT, true, false);
        let d = m.on_key(vk::SPACE, true, false);
        assert_eq!(d, Decision { event: Some(KeyEvent::Pressed), swallow: true });
        // Auto-repeat is swallowed silently.
        assert_eq!(m.on_key(vk::SPACE, true, false), Decision { event: None, swallow: true });
        // Releasing a modifier first does not end the press; releasing the key does.
        assert_eq!(m.on_key(vk::RSHIFT, false, true), Decision::default());
        assert_eq!(
            m.on_key(vk::SPACE, false, true),
            Decision { event: Some(KeyEvent::Released), swallow: true }
        );
        // Extra modifier (Alt) means a different shortcut.
        m.on_key(vk::RSHIFT, true, false);
        m.on_key(vk::LMENU, true, false);
        assert_eq!(m.on_key(vk::SPACE, true, false), Decision::default());
    }

    #[test]
    fn plain_function_key_ignores_modified_presses() {
        let mut m = matcher("F9");
        assert_eq!(m.on_key(vk::F1 + 8, true, false).event, Some(KeyEvent::Pressed));
        assert_eq!(m.on_key(vk::F1 + 8, false, true).event, Some(KeyEvent::Released));
        m.on_key(vk::LCONTROL, true, false);
        assert_eq!(m.on_key(vk::F1 + 8, true, false), Decision::default());
    }

    #[test]
    fn modifier_only_detects_interference_and_never_swallows() {
        let mut m = matcher("RightCtrl");
        assert_eq!(
            m.on_key(vk::RCONTROL, true, false),
            Decision { event: Some(KeyEvent::Pressed), swallow: false }
        );
        assert_eq!(m.on_key(vk::RCONTROL, true, true), Decision::default()); // repeat
        assert_eq!(m.on_key(0x43, true, true).event, Some(KeyEvent::Interference)); // C
        assert_eq!(m.on_key(0x56, true, true), Decision::default()); // only once
        m.on_key(0x43, false, true);
        m.on_key(0x56, false, true);
        assert_eq!(m.on_key(vk::RCONTROL, false, true).event, Some(KeyEvent::Released));
        // Pressing it while another key is already held is not a trigger.
        m.on_key(0x41, true, false);
        assert_eq!(m.on_key(vk::RCONTROL, true, false), Decision::default());
    }

    #[test]
    fn escape_cancels_only_while_recording_and_swallows_its_release() {
        let mut m = matcher("F9");
        assert_eq!(m.on_key(vk::ESCAPE, true, false), Decision::default());
        m.on_key(vk::ESCAPE, false, false);
        assert_eq!(
            m.on_key(vk::ESCAPE, true, true),
            Decision { event: Some(KeyEvent::Escape), swallow: true }
        );
        assert_eq!(m.on_key(vk::ESCAPE, false, false), Decision { event: None, swallow: true });
        assert_eq!(m.on_key(vk::ESCAPE, true, false), Decision::default());
    }

    #[test]
    fn resync_unsticks_missed_releases() {
        let mut m = matcher("F9");
        m.on_key(vk::LCONTROL, true, false); // key-up never arrives
        assert_eq!(m.on_key(vk::F1 + 8, true, false), Decision::default());
        m.on_key(vk::F1 + 8, false, false);
        m.resync(vk::F1 + 8, |_| false);
        assert_eq!(m.on_key(vk::F1 + 8, true, false).event, Some(KeyEvent::Pressed));

        // Win+L: the release of L lands on the secure desktop.
        let mut m = matcher("RightCtrl");
        m.on_key(0x4C, true, false);
        assert_eq!(m.on_key(vk::RCONTROL, true, false).event, None);
        m.on_key(vk::RCONTROL, false, false);
        m.resync(vk::RCONTROL, |_| false);
        assert_eq!(m.on_key(vk::RCONTROL, true, false).event, Some(KeyEvent::Pressed));
        // A physically held key stays held.
        m.on_key(vk::RCONTROL, false, false);
        m.on_key(vk::LSHIFT, true, false);
        m.resync(vk::RCONTROL, |c| c == vk::LSHIFT);
        assert_eq!(m.on_key(vk::RCONTROL, true, false).event, None);
    }

    #[test]
    fn reset_forgets_a_press_whose_release_was_lost() {
        let mut m = matcher("Ctrl+Shift+Space");
        m.on_key(vk::LCONTROL, true, false);
        m.on_key(vk::LSHIFT, true, false);
        assert_eq!(m.on_key(vk::SPACE, true, false).event, Some(KeyEvent::Pressed));
        m.reset(); // all key-ups happened while the hook was paused
        m.on_key(vk::LCONTROL, true, false);
        m.on_key(vk::LSHIFT, true, false);
        assert_eq!(m.on_key(vk::SPACE, true, false).event, Some(KeyEvent::Pressed));
    }

    fn auto() -> PttMachine {
        PttMachine::new(HotkeyMode::Auto, PttTiming::default())
    }

    #[test]
    fn set_mode_while_held_does_not_swallow_next_press() {
        let mut p = auto();
        assert_eq!(p.key_down(0).action, Some(PttAction::Start));
        p.set_mode(HotkeyMode::Hold); // the matcher forgets the pending release
        assert_eq!(p.key_down(5000).action, Some(PttAction::Start));
    }

    #[test]
    fn auto_hold_stops_on_release() {
        let mut p = auto();
        assert_eq!(p.key_down(0).action, Some(PttAction::Start));
        assert!(p.is_recording());
        assert_eq!(p.key_down(50), PttStep::default()); // repeat
        assert_eq!(p.key_up(1200).action, Some(PttAction::Stop));
        assert!(!p.is_recording());
    }

    #[test]
    fn auto_single_quick_tap_is_discarded_after_window() {
        let mut p = auto();
        p.key_down(0);
        let s = p.key_up(100);
        assert_eq!(s, PttStep { action: None, deadline: Some(550) });
        assert_eq!(p.timeout(400), PttStep { action: None, deadline: Some(550) });
        assert_eq!(p.timeout(550).action, Some(PttAction::Discard));
        assert!(!p.is_recording());
    }

    #[test]
    fn auto_double_tap_latches_until_next_press() {
        let mut p = auto();
        p.key_down(0);
        p.key_up(100);
        assert_eq!(p.key_down(300), PttStep::default()); // latch, keep recording
        assert_eq!(p.key_up(350), PttStep::default());
        assert!(p.is_recording());
        assert_eq!(p.timeout(1000), PttStep::default());
        // Bounce right after latching is ignored.
        p.key_down(400);
        p.key_up(420);
        assert!(p.is_recording());
        assert_eq!(p.key_down(5000).action, Some(PttAction::Stop));
        assert_eq!(p.key_up(5100), PttStep::default());
        assert!(!p.is_recording());
    }

    #[test]
    fn auto_late_second_press_starts_fresh() {
        let mut p = auto();
        p.key_down(0);
        p.key_up(100);
        // Timer not delivered yet, but the window (until 550) is over.
        assert_eq!(p.key_down(900).action, Some(PttAction::Start));
        assert_eq!(p.key_up(1500).action, Some(PttAction::Stop));
    }

    #[test]
    fn hold_mode_stops_even_on_short_press() {
        let mut p = PttMachine::new(HotkeyMode::Hold, PttTiming::default());
        assert_eq!(p.key_down(0).action, Some(PttAction::Start));
        assert_eq!(p.key_up(30).action, Some(PttAction::Stop));
    }

    #[test]
    fn toggle_mode_starts_and_stops_on_presses() {
        let mut p = PttMachine::new(HotkeyMode::Toggle, PttTiming::default());
        assert_eq!(p.key_down(0).action, Some(PttAction::Start));
        assert_eq!(p.key_up(2000), PttStep::default());
        assert_eq!(p.key_down(4000).action, Some(PttAction::Stop));
        assert_eq!(p.key_up(4100), PttStep::default());
        assert!(!p.is_recording());
    }

    #[test]
    fn interference_discards_held_recording_only() {
        let mut p = auto();
        p.key_down(0);
        assert_eq!(p.interference().action, Some(PttAction::Discard));
        assert!(!p.is_recording());

        let mut t = PttMachine::new(HotkeyMode::Toggle, PttTiming::default());
        t.key_down(0);
        assert_eq!(t.interference().action, Some(PttAction::Discard)); // key still held
        t.key_up(10);
        t.key_down(1000);
        t.key_up(1010);
        t.key_down(2000);
        assert_eq!(t.key_up(2010), PttStep::default());
    }

    #[test]
    fn cancel_discards_any_active_recording() {
        let mut p = auto();
        assert_eq!(p.cancel(), PttStep::default());
        p.key_down(0);
        p.key_up(100);
        p.key_down(200);
        assert_eq!(p.cancel().action, Some(PttAction::Discard));
        assert!(!p.is_recording());
        // The release of the latching press after cancel does nothing.
        assert_eq!(p.key_up(300), PttStep::default());
    }

    #[test]
    fn finish_stops_latched_held_or_tapped_recording() {
        let mut p = auto();
        assert_eq!(p.finish(), PttStep::default());
        // Latched by a double tap, then the indicator is clicked.
        p.key_down(0);
        p.key_up(100);
        p.key_down(200);
        p.key_up(260);
        assert_eq!(p.finish().action, Some(PttAction::Stop));
        assert!(!p.is_recording());
        assert_eq!(p.key_down(400).action, Some(PttAction::Start)); // a new dictation
        // Still holding the shortcut: its release afterwards does nothing.
        let mut held = auto();
        held.key_down(0);
        assert_eq!(held.finish().action, Some(PttAction::Stop));
        assert_eq!(held.key_up(900), PttStep::default());
        // Waiting for a possible second tap.
        let mut tap = auto();
        tap.key_down(0);
        tap.key_up(100);
        assert_eq!(tap.finish().action, Some(PttAction::Stop));
        assert_eq!(tap.timeout(550), PttStep::default());
        // Toggle mode.
        let mut toggle = PttMachine::new(HotkeyMode::Toggle, PttTiming::default());
        toggle.key_down(0);
        toggle.key_up(80);
        assert_eq!(toggle.finish().action, Some(PttAction::Stop));
        assert_eq!(toggle.key_down(1000).action, Some(PttAction::Start));
    }
}

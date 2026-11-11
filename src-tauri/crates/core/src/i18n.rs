//! Language of the app's own texts (settings window, indicator, tray, errors).
//! Dictation content (prompts, dictionary, STT language) never depends on it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiLanguage {
    #[default]
    En,
    Pl,
}

impl UiLanguage {
    /// Settings saved before the language setting existed were made with the Polish UI.
    pub fn legacy() -> Self {
        UiLanguage::Pl
    }
}

#[cfg(not(test))]
static POLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switch the language of texts made from now on (the app calls it on load and save).
#[cfg(not(test))]
pub fn set(lang: UiLanguage) {
    POLISH.store(lang == UiLanguage::Pl, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
pub fn is_polish() -> bool {
    POLISH.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    // Per thread in tests, so parallel tests can each pick a language.
    static POLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn set(lang: UiLanguage) {
    POLISH.with(|p| p.set(lang == UiLanguage::Pl));
}

#[cfg(test)]
pub fn is_polish() -> bool {
    POLISH.with(|p| p.get())
}

/// The text in the current UI language: `tr!("Pasted {n}", "Wklejono {n}")`.
/// Both literals may name variables in scope, like `format!`.
#[macro_export]
macro_rules! tr {
    ($en:literal, $pl:literal $(,)?) => {
        if $crate::i18n::is_polish() {
            format!($pl)
        } else {
            format!($en)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_follows_the_language() {
        let n = 3;
        set(UiLanguage::Pl);
        assert_eq!(tr!("{n} days", "{n} dni"), "3 dni");
        set(UiLanguage::En);
        assert_eq!(tr!("{n} days", "{n} dni"), "3 days");
    }
}

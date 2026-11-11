//! HyperRust core: everything on the dictation path that does not need a GUI.
//!
//! The Tauri app crate adds the Windows integration (keyboard hook, microphone,
//! overlay, paste). Everything here builds and tests on any platform, which is
//! also what the headless smoke test (`examples/smoke.rs`) runs, plus the
//! update-manifest checks used by the self-updater.

pub mod audio;
pub mod balance;
pub mod catalog;
pub mod config;
pub mod cue;
pub mod dictionary;
pub mod error;
pub mod history;
pub mod hotkey;
pub mod http;
pub mod i18n;
pub mod llm;
pub mod modes;
pub mod ranking;
pub mod session;
pub mod stt;
pub mod timing;
pub mod update;

pub use config::Settings;
pub use error::{Error, Result};

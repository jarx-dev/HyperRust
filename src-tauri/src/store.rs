//! Settings file (`settings.json`); never contains keys.

use hyperrust_core::Settings;

use crate::paths;

pub fn load() -> Settings {
    match std::fs::read_to_string(paths::settings_file()) {
        Ok(json) => Settings::from_json(&json),
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = paths::settings_file();
    std::fs::create_dir_all(paths::config_dir()).map_err(|e| e.to_string())?;
    hyperrust_core::update::write_atomically(&path, settings.to_json().as_bytes())
        .map_err(|e| format!("zapis ustawień do {}: {e}", path.display()))
}

//! API keys in the OS credential store (Windows Credential Manager), cached in
//! memory so the dictation path never waits for the store.

use std::collections::HashMap;
use std::sync::Mutex;

use hyperrust_core::config::SecretKey;

const SERVICE: &str = "HyperRust";

#[derive(Default)]
pub struct Secrets {
    cache: Mutex<HashMap<SecretKey, Option<String>>>,
}

fn entry(key: SecretKey) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, key.account()).map_err(|e| e.to_string())
}

impl Secrets {
    pub fn get(&self, key: SecretKey) -> Option<String> {
        let mut cache = self.cache.lock().expect("secrets mutex");
        cache
            .entry(key)
            .or_insert_with(|| {
                entry(key)
                    .ok()
                    .and_then(|e| e.get_password().ok())
                    .filter(|v| !v.trim().is_empty())
            })
            .clone()
    }

    pub fn is_set(&self, key: SecretKey) -> bool {
        self.get(key).is_some()
    }

    pub fn set(&self, key: SecretKey, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value.is_empty() {
            return self.delete(key);
        }
        entry(key)?
            .set_password(value)
            .map_err(|e| format!("nie udało się zapisać klucza w Menedżerze poświadczeń: {e}"))?;
        self.cache.lock().expect("secrets mutex").insert(key, Some(value.to_string()));
        Ok(())
    }

    pub fn delete(&self, key: SecretKey) -> Result<(), String> {
        match entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(format!("nie udało się usunąć klucza: {e}")),
        }
        self.cache.lock().expect("secrets mutex").insert(key, None);
        Ok(())
    }

    /// Warm the cache at start-up (Credential Manager reads take a few ms).
    pub fn preload(&self) {
        for key in SecretKey::ALL {
            let _ = self.get(key);
        }
    }
}

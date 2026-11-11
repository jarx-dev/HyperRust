//! Launch at login via the HKCU `Run` key (Windows only).

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager ("Startup apps") and Settings keep the user's on/off
/// choice for each `Run` entry.
#[cfg(windows)]
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "HyperRust";

/// Write or remove the `Run` entry. `user_choice` marks an explicit toggle in
/// the app, which also lifts a "disabled" set in Task Manager; the sync at
/// start-up (which refreshes the EXE path) keeps that choice.
#[cfg(windows)]
pub fn set_enabled(enabled: bool, user_choice: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu.create_subkey(RUN_KEY).map_err(|e| e.to_string())?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        run.set_value(VALUE_NAME, &format!("\"{}\"", exe.display()))
            .map_err(|e| e.to_string())?;
        if user_choice {
            // Without an entry here Windows treats the app as enabled.
            if let Ok(approved) = hkcu.open_subkey_with_flags(APPROVED_KEY, KEY_SET_VALUE) {
                let _ = approved.delete_value(VALUE_NAME);
            }
        }
    } else {
        let _ = run.delete_value(VALUE_NAME);
    }
    Ok(())
}

/// Whether Windows will start the app at login: the `Run` entry exists and is
/// not switched off in Task Manager (odd first byte of the approval value).
#[cfg(windows)]
pub fn is_active() -> Option<bool> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let registered = hkcu.open_subkey(RUN_KEY).and_then(|k| k.get_raw_value(VALUE_NAME)).is_ok();
    let disabled = hkcu
        .open_subkey(APPROVED_KEY)
        .and_then(|k| k.get_raw_value(VALUE_NAME))
        .is_ok_and(|v| v.bytes.first().is_some_and(|b| b & 1 == 1));
    Some(registered && !disabled)
}

#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool, _user_choice: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub fn is_active() -> Option<bool> {
    None
}

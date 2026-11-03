use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "MicHost";

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("resolving the executable path: {error}"))?;
    let command = format!("\"{}\" --autostart", current.display());
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)
        .map_err(|error| format!("opening the login items registry key: {error}"))?;
    if enabled {
        key.set_value(VALUE_NAME, &command)
            .map_err(|error| format!("enabling launch at login: {error}"))?;
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("disabling launch at login: {error}")),
        }
    }
    Ok(())
}

pub fn is_enabled() -> bool {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
    else {
        return false;
    };
    key.get_value::<String, _>(VALUE_NAME).is_ok()
}

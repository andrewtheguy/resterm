const SERVICE: &str = "resterm";

pub(crate) fn init_store() -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Ok(store) = apple_native_keyring_store::keychain::Store::new() {
            keyring_core::set_default_store(store);
            return true;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(store) = dbus_secret_service_keyring_store::Store::new() {
            keyring_core::set_default_store(store);
            return true;
        }
    }
    false
}

pub(crate) fn save_passphrase(instance: &str, passphrase: &str) -> Result<(), String> {
    let entry =
        keyring_core::Entry::new(SERVICE, instance).map_err(|e| format!("keychain entry: {e}"))?;
    entry
        .set_password(passphrase)
        .map_err(|e| format!("keychain save: {e}"))
}

pub(crate) fn load_passphrase(instance: &str) -> Option<String> {
    let entry = keyring_core::Entry::new(SERVICE, instance).ok()?;
    entry.get_password().ok()
}

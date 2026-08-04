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
    // Windows Credential Manager (the "Generic Credentials" vault). Entries
    // land under a `resterm.<instance>` target name and are readable only by
    // the logged-in user, same trust model as the macOS/Linux stores.
    #[cfg(target_os = "windows")]
    {
        if let Ok(store) = windows_native_keyring_store::Store::new() {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Round-trips a throwaway secret through the real OS credential store.
    // #[ignore] because it writes to the user's keychain/Credential Manager
    // and, on Linux, needs an unlocked Secret Service session.
    #[test]
    #[ignore]
    fn live_keychain_round_trip() {
        assert!(init_store(), "no native credential store available");
        let instance = format!("resterm-test-{}", std::process::id());
        save_passphrase(&instance, "correct horse battery staple").expect("save");
        assert_eq!(
            load_passphrase(&instance).as_deref(),
            Some("correct horse battery staple")
        );

        keyring_core::Entry::new(SERVICE, &instance)
            .expect("entry")
            .delete_credential()
            .expect("cleanup");
        assert!(load_passphrase(&instance).is_none(), "entry should be gone");
    }
}

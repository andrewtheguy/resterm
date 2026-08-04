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

    /// Deletes the test credential on unwind, so a failing assertion can't
    /// leave a stray entry behind in the user's real credential store.
    struct Cleanup(String);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            // Best-effort and deliberately silent: on the happy path the test
            // has already deleted the entry, so the expected outcome here is a
            // "no such credential" error. Panicking in Drop during an unwind
            // would abort the process and hide the real failure.
            if let Ok(entry) = keyring_core::Entry::new(SERVICE, &self.0) {
                let _ = entry.delete_credential();
            }
        }
    }

    // Round-trips a throwaway secret through the real OS credential store.
    // #[ignore] because it writes to the user's keychain/Credential Manager
    // and, on Linux, needs an unlocked Secret Service session.
    #[test]
    #[ignore]
    fn live_keychain_round_trip() {
        assert!(init_store(), "no native credential store available");
        let instance = format!("resterm-test-{}", std::process::id());
        save_passphrase(&instance, "correct horse battery staple").expect("save");
        // Armed the moment the entry exists — every assertion below is now
        // covered whether it passes or panics.
        let _cleanup = Cleanup(instance.clone());
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

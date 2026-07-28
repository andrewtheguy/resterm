use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, KeyInit, Mac};
use scrypt::Params as ScryptParams;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassphrasePhase {
    Setup,
    Unlock,
}

const SCRYPT_LOG_N: u8 = 16;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

pub(crate) fn derive_config_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    let params = ScryptParams::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P)
        .map_err(|e| format!("invalid scrypt parameters: {e}"))?;
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, &mut key)
        .map_err(|e| format!("scrypt failed: {e}"))?;
    Ok(key)
}

pub(crate) fn passphrase_policy_error(passphrase: &str) -> Option<&'static str> {
    if passphrase.chars().count() < 12 {
        return Some("Must be at least 12 characters.");
    }
    if !passphrase.chars().any(|c| c.is_ascii_lowercase()) {
        return Some("Must contain a lowercase letter.");
    }
    if !passphrase.chars().any(|c| c.is_ascii_uppercase()) {
        return Some("Must contain an uppercase letter.");
    }
    if !passphrase.chars().any(|c| c.is_ascii_digit()) {
        return Some("Must contain a digit.");
    }
    if !passphrase.chars().any(|c| c.is_ascii_punctuation()) {
        return Some("Must contain a special character.");
    }
    None
}

pub(crate) fn verify_instance_sig(instance: &str, key: &[u8; 32], expected_sig_b64: &str) -> bool {
    let Ok(expected) = BASE64.decode(expected_sig_b64) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(instance.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

pub(crate) fn compute_instance_sig(instance: &str, key: &[u8; 32]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(instance.as_bytes());
    BASE64.encode(mac.finalize().into_bytes())
}

pub(crate) fn derive_share_signing_key(seed: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"resterm-share-v1\0");
    hasher.update(seed);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passphrase_policy_requires_each_character_class() {
        for passphrase in [
            "short1!",
            "missingdigit!",
            "missing-special1",
            "MISSINGLOWER1!",
            "missingupper1!",
            "ValidPass123 ",
        ] {
            assert!(
                passphrase_policy_error(passphrase).is_some(),
                "passphrase should fail policy: {passphrase}"
            );
        }
        assert!(passphrase_policy_error("ValidPass123!").is_none());
    }

    #[test]
    fn verify_instance_sig_checks_key_and_instance() {
        let key = [0x42; 32];
        let sig = compute_instance_sig("mysite", &key);
        assert!(verify_instance_sig("mysite", &key, &sig));
        assert!(!verify_instance_sig("other", &key, &sig));
        assert!(!verify_instance_sig("mysite", &[0x43; 32], &sig));
        assert!(!verify_instance_sig("mysite", &key, "not base64"));
    }

    #[test]
    fn derive_share_key_is_stable() {
        let a = derive_share_signing_key(&[0x11; 32]);
        let b = derive_share_signing_key(&[0x11; 32]);
        let c = derive_share_signing_key(&[0x22; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}

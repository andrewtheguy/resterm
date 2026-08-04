use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::crypto::{Cipher, is_passphrase_encrypted};

const CONFIG_DIR_NAME: &str = "resterm";
const CONFIG_FILE: &str = "config.toml";
const LOCK_FILE: &str = "config.lock";
const CONFIG_VERSION: u32 = 2;

pub const CIPHER_MARKER_PASSPHRASE: &str = "passphrase-v1";

#[derive(Debug, Clone, Copy)]
pub enum BackendKind {
    Local,
    Rest,
    S3,
}

impl BackendKind {
    pub fn label(self) -> &'static str {
        match self {
            BackendKind::Local => "Local filesystem",
            BackendKind::Rest => "REST server",
            BackendKind::S3 => "S3 (any S3-compatible endpoint)",
        }
    }
}

/// Profile body — the name is the key in [`Config::profiles`] and is not
/// duplicated inside the variant. The map key is the primary identifier;
/// renaming = remove old key + insert new key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum Profile {
    Local {
        password: String,
        local_path: String,
    },
    Rest {
        password: String,
        rest_url: String,
        #[serde(default)]
        rest_user: String,
        #[serde(default)]
        rest_password: String,
    },
    S3 {
        password: String,
        s3_endpoint: String,
        s3_bucket: String,
        s3_region: String,
        #[serde(default)]
        s3_root: String,
        s3_access_key: String,
        s3_secret_key: String,
    },
}

impl Profile {
    pub fn password(&self) -> &str {
        match self {
            Profile::Local { password, .. }
            | Profile::Rest { password, .. }
            | Profile::S3 { password, .. } => password,
        }
    }

    pub fn backend_kind(&self) -> BackendKind {
        match self {
            Profile::Local { .. } => BackendKind::Local,
            Profile::Rest { .. } => BackendKind::Rest,
            Profile::S3 { .. } => BackendKind::S3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub cipher: String,
    pub version: u32,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<PassphraseMeta>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles: BTreeMap::new(),
            passphrase: None,
        }
    }
}

impl Config {
    pub fn has_profile(&self, name: &str) -> bool {
        self.profiles.contains_key(name)
    }

    pub fn profile_at(&self, idx: usize) -> Option<(&String, &Profile)> {
        self.profiles.iter().nth(idx)
    }

    pub fn name_at(&self, idx: usize) -> Option<&String> {
        self.profiles.keys().nth(idx)
    }
}

pub struct Paths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub lock: PathBuf,
}

/// `dirs::config_dir()` is the per-user, per-platform config root:
/// `~/.config` on Linux, `~/Library/Application Support` on macOS, and
/// `%APPDATA%` (`C:\Users\<user>\AppData\Roaming`) on Windows.
pub fn paths(override_dir: Option<PathBuf>) -> Result<Paths> {
    let base = match override_dir {
        Some(p) => p,
        None => dirs::config_dir()
            .ok_or_else(|| anyhow!("could not determine config directory"))?
            .join(CONFIG_DIR_NAME),
    };
    Ok(Paths {
        config: base.join(CONFIG_FILE),
        lock: base.join(LOCK_FILE),
        dir: base,
    })
}

/// Restrict a file to its owner where the platform can express that. On Unix
/// this is mode 0600. Windows has no chmod: a newly created file inherits the
/// ACL of its parent directory, and the config directory lives under the
/// per-user profile (`%APPDATA%`), which already grants only the owner (plus
/// SYSTEM/Administrators) access — so there is nothing to tighten there.
fn owner_only(opts: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

/// Exclusive lock over one config directory, held for the lifetime of the
/// process.
///
/// resterm keeps the whole config in memory and rewrites the file wholesale on
/// every save, so two instances pointed at the same directory would silently
/// overwrite each other's profiles. The second instance is refused instead.
///
/// Backed by [`std::fs::File::try_lock`] (stable since Rust 1.89), which is
/// `flock(LOCK_EX | LOCK_NB)` on Unix and `LockFileEx` on Windows — no extra
/// crate, and no stale-PID-file guessing, because the OS releases the lock
/// when the owning process exits for any reason, including a crash.
#[derive(Debug)]
pub struct ConfigLock {
    file: fs::File,
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // Closing the handle would release it anyway; being explicit keeps the
        // release visible at the point the guard dies.
        let _ = self.file.unlock();
    }
}

pub fn acquire_lock(paths: &Paths) -> Result<ConfigLock> {
    fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating {}", paths.dir.display()))?;
    // `truncate(false)`: the lock file is a zero-byte marker that outlives the
    // process, and truncating it is not the point — holding it is.
    let file = owner_only(
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false),
    )
    .open(&paths.lock)
    .with_context(|| format!("opening lock file {}", paths.lock.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(ConfigLock { file }),
        Err(fs::TryLockError::WouldBlock) => bail!(
            "another resterm instance is already using {} — close it first, \
             or pass --config-dir to use a different config directory",
            paths.dir.display()
        ),
        Err(fs::TryLockError::Error(e)) => {
            Err(anyhow!(e)).with_context(|| format!("locking {}", paths.lock.display()))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassphraseMeta {
    pub instance: String,
    /// base64-encoded HMAC-SHA256(instance, derived_key). Verified on
    /// unlock to give a fast "wrong passphrase" error before attempting
    /// full config decryption.
    pub instance_sig: String,
    /// base64-encoded random 32-byte scrypt salt.
    pub salt: String,
}

/// Parse config.toml without touching any encrypted fields. Returns `None`
/// when the file doesn't exist (fresh dir).
pub fn peek(paths: &Paths) -> Result<Option<Config>> {
    if !paths.config.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&paths.config)
        .with_context(|| format!("reading {}", paths.config.display()))?;
    let config: Config = toml::from_str(&text)
        .with_context(|| format!("parsing TOML from {}", paths.config.display()))?;
    Ok(Some(config))
}

/// Load the config. If the config file does not exist, returns a default
/// (empty) config without touching disk.
pub fn load(paths: &Paths, cipher: &Cipher) -> Result<Config> {
    if !paths.config.exists() {
        return Ok(Config::default());
    }

    let text = fs::read_to_string(&paths.config)
        .with_context(|| format!("reading {}", paths.config.display()))?;
    let mut config: Config = toml::from_str(&text)
        .with_context(|| format!("parsing TOML from {}", paths.config.display()))?;

    if config.cipher != CIPHER_MARKER_PASSPHRASE {
        bail!(
            "{} has unsupported cipher = \"{}\"; only \"{}\" is supported",
            paths.config.display(),
            config.cipher,
            CIPHER_MARKER_PASSPHRASE
        );
    }

    if config.version != CONFIG_VERSION {
        bail!(
            "config at {} has version {} but this build of resterm expects version {} \
             (no migrations are supported — this is a personal tool with no backwards compatibility)",
            paths.config.display(),
            config.version,
            CONFIG_VERSION
        );
    }

    for (name, profile) in &mut config.profiles {
        decrypt_profile_fields(profile, cipher)
            .with_context(|| format!("decrypting profile `{name}`"))?;
    }
    Ok(config)
}

/// Encrypt and write the config atomically using the active cipher.
pub fn save(config: &Config, paths: &Paths, cipher: &Cipher) -> Result<()> {
    let mut on_disk = config.clone();
    on_disk.cipher = CIPHER_MARKER_PASSPHRASE.to_string();
    on_disk.version = CONFIG_VERSION;
    for (name, profile) in &mut on_disk.profiles {
        encrypt_profile_fields(profile, cipher)
            .with_context(|| format!("encrypting profile `{name}`"))?;
    }

    let text = toml::to_string_pretty(&on_disk).context("serializing config to TOML")?;

    if let Some(parent) = paths.config.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let tmp = paths.config.with_extension("toml.tmp");
    {
        let mut file = owner_only(
            fs::OpenOptions::new().write(true).create(true).truncate(true),
        )
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all().ok();
    }
    // The temp file's handle is closed above — Windows refuses to rename over a
    // destination while any handle to the source is open. `fs::rename` replaces
    // an existing destination on both platforms (`MoveFileEx` with
    // MOVEFILE_REPLACE_EXISTING on Windows).
    fs::rename(&tmp, &paths.config)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), paths.config.display()))?;
    Ok(())
}

fn encrypt_field(value: &mut String, cipher: &Cipher) -> Result<()> {
    if value.is_empty() || is_passphrase_encrypted(value) {
        return Ok(());
    }
    *value = cipher.encrypt(value)?;
    Ok(())
}

fn decrypt_field(value: &mut String, cipher: &Cipher) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    *value = cipher.decrypt(value)?;
    Ok(())
}

fn encrypt_profile_fields(profile: &mut Profile, cipher: &Cipher) -> Result<()> {
    match profile {
        Profile::Local { password, .. } => {
            encrypt_field(password, cipher)?;
        }
        Profile::Rest {
            password,
            rest_password,
            ..
        } => {
            encrypt_field(password, cipher)?;
            encrypt_field(rest_password, cipher)?;
        }
        Profile::S3 {
            password,
            s3_secret_key,
            ..
        } => {
            encrypt_field(password, cipher)?;
            encrypt_field(s3_secret_key, cipher)?;
        }
    }
    Ok(())
}

fn decrypt_profile_fields(profile: &mut Profile, cipher: &Cipher) -> Result<()> {
    match profile {
        Profile::Local { password, .. } => {
            decrypt_field(password, cipher)?;
        }
        Profile::Rest {
            password,
            rest_password,
            ..
        } => {
            decrypt_field(password, cipher)?;
            decrypt_field(rest_password, cipher)?;
        }
        Profile::S3 {
            password,
            s3_secret_key,
            ..
        } => {
            decrypt_field(password, cipher)?;
            decrypt_field(s3_secret_key, cipher)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from("tmp").join(format!("cfgtest-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn test_paths(dir: &std::path::Path) -> Paths {
        Paths {
            dir: dir.to_path_buf(),
            config: dir.join(CONFIG_FILE),
            lock: dir.join(LOCK_FILE),
        }
    }

    #[test]
    fn round_trip_encrypt_decrypt() -> Result<()> {
        let dir = fresh_dir("rt");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");

        let empty = load(&paths, &cipher)?;
        assert_eq!(empty.profiles.len(), 0);
        assert_eq!(empty.version, CONFIG_VERSION);

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "local-a".to_string(),
            Profile::Local {
                password: "pw1".into(),
                local_path: "/var/restic/a".into(),
            },
        );
        profiles.insert(
            "s3-b".to_string(),
            Profile::S3 {
                password: "pw2".into(),
                s3_endpoint: "https://s3.example.com".into(),
                s3_bucket: "buk".into(),
                s3_region: "us-east-1".into(),
                s3_root: "/sub/dir".into(),
                s3_access_key: "AK".into(),
                s3_secret_key: "SK".into(),
            },
        );
        profiles.insert(
            "rest-c".to_string(),
            Profile::Rest {
                password: "pw3".into(),
                rest_url: "https://r.example.com/repo".into(),
                rest_user: String::new(),
                rest_password: String::new(),
            },
        );
        let cfg = Config {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles,
            passphrase: None,
        };
        save(&cfg, &paths, &cipher)?;
        assert!(paths.config.exists());

        let raw = fs::read_to_string(&paths.config)?;
        let parsed: toml::Value = toml::from_str(&raw)?;
        assert_eq!(parsed["cipher"].as_str().unwrap(), CIPHER_MARKER_PASSPHRASE);

        let table = parsed["profiles"].as_table().unwrap();
        assert_eq!(table.len(), 3);
        for (name, profile) in table {
            let profile = profile.as_table().unwrap();
            let password = profile["password"].as_str().unwrap();
            assert!(crate::crypto::is_passphrase_encrypted(password), "password should be encrypted: {password}");
            assert!(!password.contains('\n'), "encrypted value must be single-line");
            assert!(!profile.contains_key("name"), "`name` should not be inside profile `{name}`");
            if profile["backend"].as_str().unwrap() == "local" {
                assert!(!crate::crypto::is_passphrase_encrypted(profile["local_path"].as_str().unwrap()));
            }
        }

        let loaded = load(&paths, &cipher)?;
        assert_eq!(loaded.profiles.len(), 3);
        match loaded.profiles.get("local-a") {
            Some(Profile::Local { password, local_path }) => {
                assert_eq!(password, "pw1");
                assert_eq!(local_path, "/var/restic/a");
            }
            other => panic!("expected Local, got {other:?}"),
        }
        match loaded.profiles.get("s3-b") {
            Some(Profile::S3 { s3_root, s3_access_key, s3_secret_key, .. }) => {
                assert_eq!(s3_root, "/sub/dir");
                assert_eq!(s3_access_key, "AK");
                assert_eq!(s3_secret_key, "SK");
            }
            other => panic!("expected S3, got {other:?}"),
        }
        match loaded.profiles.get("rest-c") {
            Some(Profile::Rest { rest_url, .. }) => {
                assert_eq!(rest_url, "https://r.example.com/repo");
            }
            other => panic!("expected Rest, got {other:?}"),
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn rest_round_trip_with_split_auth() -> Result<()> {
        let dir = fresh_dir("rest_split");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "rest-auth".to_string(),
            Profile::Rest {
                password: "repo-pw".into(),
                rest_url: "https://r.example.com/repo/".into(),
                rest_user: "andrew".into(),
                rest_password: "hunter2".into(),
            },
        );
        let cfg = Config {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles,
            passphrase: None,
        };
        save(&cfg, &paths, &cipher)?;

        let raw = fs::read_to_string(&paths.config)?;
        let parsed: toml::Value = toml::from_str(&raw)?;
        let rest = &parsed["profiles"]["rest-auth"];
        assert_eq!(rest["rest_url"].as_str().unwrap(), "https://r.example.com/repo/");
        assert_eq!(rest["rest_user"].as_str().unwrap(), "andrew");
        assert!(crate::crypto::is_passphrase_encrypted(rest["rest_password"].as_str().unwrap()));

        let loaded = load(&paths, &cipher)?;
        match loaded.profiles.get("rest-auth") {
            Some(Profile::Rest {
                rest_url,
                rest_user,
                rest_password,
                password,
            }) => {
                assert_eq!(rest_url, "https://r.example.com/repo/");
                assert_eq!(rest_user, "andrew");
                assert_eq!(rest_password, "hunter2");
                assert_eq!(password, "repo-pw");
            }
            other => panic!("expected Rest, got {other:?}"),
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn empty_string_field_not_encrypted() -> Result<()> {
        let dir = fresh_dir("empty");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "anon".to_string(),
            Profile::Rest {
                password: "pw".into(),
                rest_url: "http://localhost:8000/".into(),
                rest_user: String::new(),
                rest_password: String::new(),
            },
        );
        let cfg = Config {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles,
            passphrase: None,
        };
        save(&cfg, &paths, &cipher)?;

        let raw = fs::read_to_string(&paths.config)?;
        let parsed: toml::Value = toml::from_str(&raw)?;
        let rest = &parsed["profiles"]["anon"];
        assert_eq!(rest["rest_user"].as_str().unwrap(), "");
        assert_eq!(rest["rest_password"].as_str().unwrap(), "");

        let loaded = load(&paths, &cipher)?;
        match loaded.profiles.get("anon") {
            Some(Profile::Rest {
                rest_user,
                rest_password,
                ..
            }) => {
                assert!(rest_user.is_empty());
                assert!(rest_password.is_empty());
            }
            other => panic!("expected Rest, got {other:?}"),
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn paths_uses_override_when_provided() -> Result<()> {
        let dir = fresh_dir("override");
        let p = paths(Some(dir.clone()))?;
        assert_eq!(p.dir, dir);
        assert_eq!(p.config, dir.join("config.toml"));
        assert_eq!(p.lock, dir.join("config.lock"));
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() -> Result<()> {
        let dir = fresh_dir("lock");
        // `acquire_lock` creates the directory itself — remove it first so the
        // test covers a genuinely fresh config dir.
        fs::remove_dir_all(&dir).ok();
        let paths = test_paths(&dir);

        let first = acquire_lock(&paths)?;
        assert!(paths.lock.exists(), "lock file should be created");

        let err = acquire_lock(&paths).expect_err("second lock must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("another resterm instance"),
            "error should name the conflict: {msg}"
        );

        drop(first);
        // Same process, but a fresh handle: flock/LockFileEx are per-handle, so
        // this proves the first guard actually released.
        let again = acquire_lock(&paths)?;
        drop(again);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn save_and_lock_coexist_in_one_directory() -> Result<()> {
        let dir = fresh_dir("lock_save");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x11u8; 32], "test".into(), "testsig0");

        let guard = acquire_lock(&paths)?;
        save(&Config::default(), &paths, &cipher)?;
        // Saving twice exercises the rename-over-existing path, which is the
        // one that differs on Windows.
        save(&Config::default(), &paths, &cipher)?;
        assert_eq!(load(&paths, &cipher)?.version, CONFIG_VERSION);
        drop(guard);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn load_rejects_unknown_version() -> Result<()> {
        let dir = fresh_dir("ver");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");

        save(&Config::default(), &paths, &cipher)?;
        let raw = fs::read_to_string(&paths.config)?;
        let mut doc: toml::Value = toml::from_str(&raw)?;
        doc["version"] = toml::Value::Integer((CONFIG_VERSION + 1) as i64);
        fs::write(&paths.config, toml::to_string_pretty(&doc)?)?;

        let err = load(&paths, &cipher).expect_err("version mismatch should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("version") && msg.contains(&(CONFIG_VERSION + 1).to_string()),
            "error should mention version mismatch: {msg}"
        );

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn load_rejects_unsupported_cipher() -> Result<()> {
        let dir = fresh_dir("bad_cipher");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");
        save(&Config::default(), &paths, &cipher)?;

        let raw = fs::read_to_string(&paths.config)?;
        let mut doc: toml::Value = toml::from_str(&raw)?;
        doc["cipher"] = toml::Value::String("age-v1".into());
        fs::write(&paths.config, toml::to_string_pretty(&doc)?)?;

        let err = load(&paths, &cipher).expect_err("unsupported cipher should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("unsupported cipher"), "error should mention unsupported cipher: {msg}");
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn passphrase_round_trip_per_value() -> Result<()> {
        let dir = fresh_dir("pp_rt");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0x55u8; 32], "test".into(), "testsig0");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "pp-local".to_string(),
            Profile::Local {
                password: "pw".into(),
                local_path: "/x".into(),
            },
        );
        let cfg = Config {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles,
            passphrase: None,
        };
        save(&cfg, &paths, &cipher)?;

        let raw = fs::read_to_string(&paths.config)?;
        let parsed: toml::Value = toml::from_str(&raw)?;
        assert_eq!(parsed["cipher"].as_str().unwrap(), CIPHER_MARKER_PASSPHRASE);
        let pw = parsed["profiles"]["pp-local"]["password"].as_str().unwrap();
        assert!(crate::crypto::is_passphrase_encrypted(pw), "password should be encrypted, got {pw}");

        let loaded = load(&paths, &cipher)?;
        match loaded.profiles.get("pp-local") {
            Some(Profile::Local { password, local_path }) => {
                assert_eq!(password, "pw");
                assert_eq!(local_path, "/x");
            }
            other => panic!("expected Local, got {other:?}"),
        }
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn passphrase_block_round_trips_inline() -> Result<()> {
        let dir = fresh_dir("pp_inline");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0xA5u8; 32], "test".into(), "testsig0");

        let cfg = Config {
            passphrase: Some(PassphraseMeta {
                instance: "mysite".into(),
                instance_sig: "c2ln".into(),
                salt: "c2FsdA==".into(),
            }),
            ..Config::default()
        };
        save(&cfg, &paths, &cipher)?;

        let raw = fs::read_to_string(&paths.config)?;
        let parsed: toml::Value = toml::from_str(&raw)?;
        assert_eq!(parsed["passphrase"]["instance"].as_str().unwrap(), "mysite");
        assert_eq!(parsed["passphrase"]["instance_sig"].as_str().unwrap(), "c2ln");
        assert_eq!(parsed["passphrase"]["salt"].as_str().unwrap(), "c2FsdA==");

        let loaded = load(&paths, &cipher)?;
        let m = loaded.passphrase.expect("[passphrase] block must round-trip");
        assert_eq!(m.instance, "mysite");
        assert_eq!(m.instance_sig, "c2ln");
        assert_eq!(m.salt, "c2FsdA==");
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn peek_reads_passphrase_block_without_decrypting() -> Result<()> {
        let dir = fresh_dir("pp_peek");
        let paths = test_paths(&dir);
        let cipher = Cipher::new([0xB6u8; 32], "test".into(), "testsig0");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "x".to_string(),
            Profile::Local {
                password: "secret".into(),
                local_path: "/x".into(),
            },
        );
        let mut cfg = Config {
            cipher: String::new(),
            version: CONFIG_VERSION,
            profiles,
            passphrase: Some(PassphraseMeta {
                instance: "peek".into(),
                instance_sig: "c2ln".into(),
                salt: "U0FMVA==".into(),
            }),
        };
        save(&cfg, &paths, &cipher)?;
        cfg = peek(&paths)?.expect("peek must see existing config");
        assert_eq!(cfg.cipher, CIPHER_MARKER_PASSPHRASE);
        let m = cfg.passphrase.expect("[passphrase] block visible to peek");
        assert_eq!(m.instance, "peek");
        match cfg.profiles.get("x") {
            Some(Profile::Local { password, .. }) => {
                assert!(crate::crypto::is_passphrase_encrypted(password));
            }
            _ => panic!("expected Local"),
        }
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn peek_returns_none_for_missing_file() -> Result<()> {
        let dir = fresh_dir("pk_peek_none");
        let paths = test_paths(&dir);
        assert!(peek(&paths)?.is_none());
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}

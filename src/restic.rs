use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Result, anyhow};
use bytes::Bytes;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config::Profile;

const MIN_MAJOR: u32 = 0;
const MIN_MINOR: u32 = 19;
const MIN_PATCH: u32 = 1;

pub(crate) struct ResticInfo;

#[derive(Debug)]
pub(crate) enum ResticError {
    NotFound,
    TooOld { found: String },
    Unparseable { output: String },
}

impl ResticError {
    pub(crate) fn user_message(&self) -> String {
        let min = format!("{MIN_MAJOR}.{MIN_MINOR}.{MIN_PATCH}");
        match self {
            ResticError::NotFound => format!(
                "restic not found on PATH. Install restic >= {min} to use wrustic."
            ),
            ResticError::TooOld { found } => format!(
                "restic {found} found on PATH, but >= {min} is required to use wrustic."
            ),
            ResticError::Unparseable { output } => {
                format!("Could not parse restic version output: {output}")
            }
        }
    }
}

pub(crate) fn detect() -> Result<ResticInfo, ResticError> {
    let output = match Command::new("restic").arg("version").output() {
        Ok(o) => o,
        Err(_) => return Err(ResticError::NotFound),
    };
    if !output.status.success() {
        return Err(ResticError::Unparseable {
            output: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // Format: "restic 0.19.1 compiled with go1.25.1 on linux/amd64"
    let version = stdout
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| ResticError::Unparseable { output: stdout.clone() })?;
    let (major, minor, patch) = parse_version(version)
        .ok_or_else(|| ResticError::Unparseable { output: stdout.clone() })?;
    if (major, minor, patch) < (MIN_MAJOR, MIN_MINOR, MIN_PATCH) {
        return Err(ResticError::TooOld { found: version.to_string() });
    }
    Ok(ResticInfo)
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    // Patch may carry a suffix like "1-dev"; take the leading digits.
    let patch_raw = it.next()?;
    let patch_digits: String = patch_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    let patch: u32 = patch_digits.parse().ok()?;
    Some((major, minor, patch))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct SnapshotDetails {
    pub(crate) id: String,
    pub(crate) short_id: Option<String>,
    pub(crate) time: Option<String>,
    pub(crate) hostname: Option<String>,
    pub(crate) username: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) paths: Vec<String>,
    pub(crate) parent: Option<String>,
    pub(crate) tree: Option<String>,
    pub(crate) program_version: Option<String>,
    pub(crate) summary: Option<SnapshotSummary>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct SnapshotSummary {
    pub(crate) backup_start: Option<String>,
    pub(crate) backup_end: Option<String>,
    pub(crate) total_files_processed: Option<u64>,
    pub(crate) total_bytes_processed: Option<u64>,
    pub(crate) data_added: Option<u64>,
    pub(crate) data_added_packed: Option<u64>,
}

/// `snapshot_id` must be the full 64-char hex hash (enforced — short ids are
/// rejected to avoid prefix ambiguity). Errors if restic returns zero or more
/// than one matching snapshot.
pub(crate) fn snapshot_details_json(
    profile: &Profile,
    snapshot_id: &str,
) -> Result<(SnapshotDetails, String)> {
    ensure_full_snapshot_id(snapshot_id)?;
    let output = run(profile, &["snapshots", snapshot_id, "--json"])?;
    let stdout = String::from_utf8_lossy(&output).into_owned();
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| anyhow!("parsing restic snapshots JSON: {e}\nraw: {stdout}"))?;
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|e| anyhow!("pretty-printing JSON: {e}"))?;
    let first = match value {
        serde_json::Value::Array(mut arr) => match arr.len() {
            0 => return Err(anyhow!("restic returned no snapshot matching `{snapshot_id}`")),
            1 => arr.remove(0),
            n => {
                return Err(anyhow!(
                    "restic returned {n} snapshots matching `{snapshot_id}`, expected exactly one"
                ));
            }
        },
        _ => return Err(anyhow!("restic snapshots JSON was not an array: {stdout}")),
    };
    let one: SnapshotDetails = serde_json::from_value(first)
        .map_err(|e| anyhow!("converting JSON value to SnapshotDetails: {e}"))?;
    Ok((one, pretty))
}

/// `snapshot_id` must be the full 64-char hex hash (enforced — short ids are
/// rejected to avoid prefix ambiguity). Returns the raw JSONL output of
/// `restic ls --json`, which lists the whole snapshot recursively in one
/// invocation.
pub(crate) fn ls_json(profile: &Profile, snapshot_id: &str) -> Result<Vec<u8>> {
    ensure_full_snapshot_id(snapshot_id)?;
    run(profile, &["ls", "--json", snapshot_id])
}

/// `snapshot_id` must be the full 64-char hex hash (enforced — short ids are
/// rejected to avoid silently forgetting the wrong snapshot when a prefix
/// matches multiple).
pub(crate) fn forget(profile: &Profile, snapshot_id: &str) -> Result<()> {
    ensure_full_snapshot_id(snapshot_id)?;
    run(profile, &["forget", snapshot_id])?;
    Ok(())
}

// Restic snapshot ids are SHA-256 hashes — 32 bytes = 64 hex chars
// (either case accepted; hex is case-insensitive). Restic's CLI accepts
// shorter prefixes, but we refuse them so callers can't accidentally act on
// the wrong snapshot if a prefix matches multiple.
fn ensure_full_snapshot_id(id: &str) -> Result<()> {
    if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(anyhow!(
            "expected a full 64-char hex snapshot id, got `{id}` (length {})",
            id.len()
        ))
    }
}

// Run `restic <args>` with credentials passed by the safest mechanism each
// supports. Never put secrets in argv. Master password is piped via the
// child's stdin (`--password-file /dev/stdin`); the repo URL and any cloud
// creds go through env vars (override-only — parent env is inherited so PATH,
// HOME, SSL_CERT_FILE, HTTP_PROXY, etc. still flow through).
pub(crate) fn command(profile: &Profile, args: &[&str]) -> Result<Command> {
    let mut cmd = Command::new("restic");
    cmd.arg("--password-file").arg("/dev/stdin");
    cmd.args(args);
    // An explicit password file wins over these in restic, but removing them
    // ensures a caller's shell cannot accidentally leak an unrelated secret
    // into the child process.
    cmd.env_remove("RESTIC_PASSWORD");
    cmd.env_remove("RESTIC_PASSWORD_FILE");
    cmd.env_remove("RESTIC_PASSWORD_COMMAND");
    cmd.env("RESTIC_REPOSITORY", repo_url(profile)?);
    match profile {
        Profile::Local { .. } | Profile::Rest { .. } => {}
        Profile::S3 {
            s3_access_key,
            s3_secret_key,
            s3_region,
            ..
        } => {
            cmd.env("AWS_ACCESS_KEY_ID", s3_access_key);
            cmd.env("AWS_SECRET_ACCESS_KEY", s3_secret_key);
            if !s3_region.is_empty() {
                cmd.env("AWS_DEFAULT_REGION", s3_region);
            }
        }
    }
    cmd.stdin(Stdio::piped());
    Ok(cmd)
}

pub(crate) fn write_password(child: &mut std::process::Child, profile: &Profile) -> Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open restic stdin"))?;
    stdin
        .write_all(profile.password().as_bytes())
        .map_err(|e| anyhow!("writing password to restic stdin: {e}"))?;
    stdin
        .write_all(b"\n")
        .map_err(|e| anyhow!("writing newline to restic stdin: {e}"))?;
    // Closing the pipe is significant: restic's password-file reader waits
    // for EOF before it can continue.
    drop(stdin);
    Ok(())
}

pub(crate) fn run(profile: &Profile, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = command(profile, args)?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to spawn `restic`: {e}"))?;
    write_password(&mut child, profile)?;
    let output = child
        .wait_with_output()
        .map_err(|e| anyhow!("waiting on restic: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "restic exited with status {}: {}",
            output.status,
            if stderr.is_empty() { "(no stderr)" } else { &stderr }
        ));
    }
    Ok(output.stdout)
}

pub(crate) fn stream_dump(
    profile: &Profile,
    snapshot_id: &str,
    path: &str,
    tx: &mpsc::Sender<std::io::Result<Bytes>>,
) -> Result<()> {
    ensure_full_snapshot_id(snapshot_id)?;
    let mut cmd = command(profile, &["dump", snapshot_id, path])?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to spawn `restic dump`: {e}"))?;
    write_password(&mut child, profile)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open restic dump stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open restic dump stderr"))?;
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.read_to_end(&mut bytes);
        (result, bytes)
    });

    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = match stdout.read(&mut buffer) {
            Ok(count) => count,
            Err(e) => {
                // Tear the child and stderr reader down before surfacing the
                // read error. Cleanup failures are swallowed so they can't
                // mask the primary error.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(anyhow!("reading restic dump stdout: {e}"));
            }
        };
        if count == 0 {
            break;
        }
        if tx
            .blocking_send(Ok(Bytes::copy_from_slice(&buffer[..count])))
            .is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Ok(());
        }
    }

    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            // Same teardown as the read-error path; the primary error wins.
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(anyhow!("waiting on restic dump: {e}"));
        }
    };
    let (stderr_result, stderr) = stderr_reader
        .join()
        .map_err(|_| anyhow!("restic dump stderr reader panicked"))?;
    stderr_result.map_err(|e| anyhow!("reading restic dump stderr: {e}"))?;
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(anyhow!(
            "restic dump exited with status {status}: {}",
            if message.is_empty() {
                "(no stderr)"
            } else {
                &message
            }
        ));
    }
    Ok(())
}

fn repo_url(profile: &Profile) -> Result<String> {
    match profile {
        Profile::Local { local_path, .. } => Ok(local_path.clone()),
        Profile::Rest {
            rest_url,
            rest_user,
            rest_password,
            ..
        } => {
            let mut url = url::Url::parse(rest_url)
                .map_err(|e| anyhow!("parsing REST URL `{rest_url}`: {e}"))?;
            if !rest_user.is_empty() {
                url.set_username(rest_user)
                    .map_err(|_| anyhow!("REST URL `{rest_url}` cannot carry a username"))?;
            }
            if !rest_password.is_empty() {
                url.set_password(Some(rest_password))
                    .map_err(|_| anyhow!("REST URL `{rest_url}` cannot carry a password"))?;
            }
            Ok(format!("rest:{url}"))
        }
        Profile::S3 {
            s3_endpoint,
            s3_bucket,
            s3_root,
            ..
        } => {
            // restic accepts `s3:<endpoint>/<bucket>[/<path>]`. When no
            // endpoint is set, default to AWS by using the bucket-only form;
            // restic's S3 backend reads the region from AWS_DEFAULT_REGION.
            let endpoint = if s3_endpoint.is_empty() {
                "s3.amazonaws.com".to_string()
            } else {
                let endpoint = s3_endpoint.trim_end_matches('/');
                if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                    endpoint.to_string()
                } else {
                    format!("https://{endpoint}")
                }
            };
            let root = s3_root.trim_matches('/');
            if root.is_empty() {
                Ok(format!("s3:{endpoint}/{s3_bucket}"))
            } else {
                Ok(format!("s3:{endpoint}/{s3_bucket}/{root}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_string() {
        assert_eq!(parse_version("0.19.1"), Some((0, 19, 1)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.19.1-dev"), Some((0, 19, 1)));
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("0.18"), None);
    }

    #[test]
    fn repo_url_local() {
        let p = Profile::Local {
            password: "pw".into(),
            local_path: "/var/restic/a".into(),
        };
        assert_eq!(repo_url(&p).unwrap(), "/var/restic/a");
    }

    #[test]
    fn repo_url_rest_no_auth() {
        let p = Profile::Rest {
            password: "pw".into(),
            rest_url: "https://r.example.com/repo/".into(),
            rest_user: String::new(),
            rest_password: String::new(),
        };
        assert_eq!(repo_url(&p).unwrap(), "rest:https://r.example.com/repo/");
    }

    #[test]
    fn repo_url_rest_with_auth() {
        let p = Profile::Rest {
            password: "pw".into(),
            rest_url: "https://r.example.com/repo/".into(),
            rest_user: "andrew".into(),
            rest_password: "hunter2".into(),
        };
        assert_eq!(
            repo_url(&p).unwrap(),
            "rest:https://andrew:hunter2@r.example.com/repo/"
        );
    }

    #[test]
    fn repo_url_s3_aws() {
        let p = Profile::S3 {
            password: "pw".into(),
            s3_endpoint: String::new(),
            s3_bucket: "my-bucket".into(),
            s3_region: "us-east-1".into(),
            s3_root: String::new(),
            s3_access_key: "AK".into(),
            s3_secret_key: "SK".into(),
        };
        assert_eq!(repo_url(&p).unwrap(), "s3:s3.amazonaws.com/my-bucket");
    }

    #[test]
    fn repo_url_s3_custom_endpoint_with_root() {
        let p = Profile::S3 {
            password: "pw".into(),
            s3_endpoint: "http://127.0.0.1:8333/".into(),
            s3_bucket: "buk".into(),
            s3_region: "us-east-1".into(),
            s3_root: "/sub/dir/".into(),
            s3_access_key: "AK".into(),
            s3_secret_key: "SK".into(),
        };
        assert_eq!(
            repo_url(&p).unwrap(),
            "s3:http://127.0.0.1:8333/buk/sub/dir"
        );
    }

    #[test]
    fn repo_url_s3_custom_endpoint_defaults_to_https() {
        let p = Profile::S3 {
            password: "pw".into(),
            s3_endpoint: "garage.example.com/".into(),
            s3_bucket: "buk".into(),
            s3_region: "garage".into(),
            s3_root: String::new(),
            s3_access_key: "AK".into(),
            s3_secret_key: "SK".into(),
        };
        assert_eq!(
            repo_url(&p).unwrap(),
            "s3:https://garage.example.com/buk"
        );
    }

    #[test]
    fn snapshot_details_deserializes() {
        let raw = r#"[{
            "id": "fullid",
            "short_id": "abcd1234",
            "time": "2025-01-01T00:00:00Z",
            "hostname": "host",
            "username": "user",
            "tags": ["weekly"],
            "paths": ["/home"],
            "parent": "parentid",
            "tree": "treeid",
            "program_version": "restic 0.19.1",
            "summary": {
                "backup_start": "2025-01-01T00:00:00Z",
                "backup_end": "2025-01-01T00:00:05Z",
                "total_files_processed": 10,
                "total_bytes_processed": 1024,
                "data_added": 512,
                "data_added_packed": 500
            }
        }]"#;
        let arr: Vec<SnapshotDetails> = serde_json::from_str(raw).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].id, "fullid");
        assert_eq!(arr[0].short_id.as_deref(), Some("abcd1234"));
        assert_eq!(arr[0].tags, vec!["weekly"]);
        let sum = arr[0].summary.as_ref().unwrap();
        assert_eq!(sum.total_files_processed, Some(10));
    }

    #[test]
    fn full_snapshot_id_accepts_64_hex() {
        let full = "ceedd62f4a63412571eac929f67931fb9702f31b681387e446e61cae3e039e73";
        assert!(ensure_full_snapshot_id(full).is_ok());
    }

    #[test]
    fn full_snapshot_id_rejects_short_and_nonhex() {
        assert!(ensure_full_snapshot_id("ceedd62f").is_err());
        assert!(ensure_full_snapshot_id("").is_err());
        // 64 chars but not all hex.
        let mut bad = "z".repeat(64);
        assert!(ensure_full_snapshot_id(&bad).is_err());
        // 63 hex chars (just one short).
        bad = "a".repeat(63);
        assert!(ensure_full_snapshot_id(&bad).is_err());
        // 65 hex chars.
        bad = "a".repeat(65);
        assert!(ensure_full_snapshot_id(&bad).is_err());
    }

    #[test]
    fn snapshot_details_tolerates_missing_summary() {
        let raw = r#"[{ "id": "id1", "paths": [], "tags": [] }]"#;
        let arr: Vec<SnapshotDetails> = serde_json::from_str(raw).unwrap();
        assert!(arr[0].summary.is_none());
    }

    // End-to-end: actually shells out to `restic` against a fresh local repo.
    // Marked #[ignore] so it doesn't run unless requested
    // (`cargo test -- --ignored`). Validates the stdin password channel, env
    // var wiring, JSON parsing, and forget — i.e. the full spawn() pipeline.
    #[test]
    #[ignore]
    fn live_restic_delete_round_trip() {
        use std::fs;
        use std::path::PathBuf;

        let root = PathBuf::from("tmp").join(format!("restic-it-{}", std::process::id()));
        let repo = root.join("repo");
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("a.txt"), b"hello\n").unwrap();

        let profile = Profile::Local {
            password: "pw".into(),
            local_path: repo.to_string_lossy().into_owned(),
        };
        run(&profile, &["init"]).expect("init");
        run(&profile, &["backup", source.to_str().unwrap()]).expect("backup");

        // List snapshots via restic CLI (also exercises stdin-password path).
        let list = run(&profile, &["snapshots", "--json"]).expect("list");
        let arr: Vec<SnapshotDetails> = serde_json::from_slice(&list).expect("parse list");
        assert_eq!(arr.len(), 1, "expected one snapshot");
        let id = arr[0].id.clone();

        // Fetch details for the specific snapshot.
        let (parsed, raw_pretty) = snapshot_details_json(&profile, &id).expect("details");
        assert_eq!(parsed.id, id);
        assert!(raw_pretty.contains(&id), "raw JSON should mention the id");

        // Forget it.
        forget(&profile, &id).expect("forget");

        // Confirm it's gone.
        let after = run(&profile, &["snapshots", "--json"]).expect("after-list");
        let arr_after: Vec<SnapshotDetails> = serde_json::from_slice(&after).expect("parse after");
        assert!(arr_after.is_empty(), "snapshot should be deleted");

        fs::remove_dir_all(&root).ok();
    }

}

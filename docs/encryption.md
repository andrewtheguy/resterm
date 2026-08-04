# Encryption

Per-value secret encryption for `config.toml`, plus passphrase input
server that derives the key.

**Scope: single-user, single-device.** resterm is a personal tool — one
person, one machine. There is no multi-user threat model, no privilege
separation inside the binary, no defense-in-depth against another local
account on the same machine. Everything below — file permissions,
localhost-only servers, in-memory key handling — is sized for that
scope. If you need a multi-tenant secret store, this isn't it.

One cipher is supported:

| Cipher | Prefix | Algorithm | Key source |
|---|---|---|---|
| `Cipher` | `$WR;1.0;AES-256-GCM;<instance>;` | AES-256-GCM AEAD | scrypt-derived config key from user passphrase, never on disk |

Source of truth: `src/crypto.rs`, `src/config.rs`, `src/passphrase.rs`.

## Why per-value, not whole-file

`config.toml` is encrypted **field by field**, not as one blob. Each secret
field is a single line, base64-after-prefix. The trade-offs:

- Non-secret edits (URLs, paths, bucket names, profile renames) still diff
  cleanly without decrypting anything.
- Adding a new field doesn't force re-encryption of every other field.
- The set of which fields are secret is hardcoded (see
  `config::encrypt_profile_fields`). New backends must opt fields in
  explicitly.

Which fields are encrypted, per backend (`src/config.rs`):

| Backend | Encrypted | Plaintext |
|---|---|---|
| `Local` | `password` | `local_path` |
| `Rest` | `password`, `rest_password` | `rest_url`, `rest_user` |
| `S3` | `password`, `s3_secret_key` | `s3_endpoint`, `s3_bucket`, `s3_region`, `s3_root`, `s3_access_key` |

The line is drawn at *credentials*, not at "anything auth-adjacent".
`rest_user` and `s3_access_key` are identifiers, not secrets: an S3 access
key ID travels in plaintext in every signed request header and shows up in
server logs, and a username is a username. Encrypting them would buy no
confidentiality while making it impossible to see at a glance which
account a profile uses. Their secret halves — `rest_password` and
`s3_secret_key` — are what actually gets encrypted.

Empty strings short-circuit: `encrypt_field` returns early on `""` so an
unset `rest_user` stays as `rest_user = ""` in the TOML rather than being
encrypted into noise.

## On-disk schema

```toml
version = 2
cipher  = "passphrase-v1"      # required, no default

[profiles.<name>]
backend  = "local" | "rest" | "s3"
password = "$WR;1.0;AES-256-GCM;mysite;…"
# plus backend-specific fields (some encrypted, some not — see table above)

[passphrase]
instance     = "<text>"     # DNS-safe instance (max 32 chars)
instance_sig = "<base64>"   # HMAC-SHA256(instance, derived_key)
salt          = "<base64>"   # random 32-byte scrypt salt
```

### Required fields and safety checks

1. **`cipher` is mandatory.** The `Config::cipher` field has no
   `#[serde(default)]`, so a TOML without that key fails to parse — there
   is no silent fallback. The only accepted value is `"passphrase-v1"`
   (constant in `src/config.rs`).
2. **`config::load` validates the marker.** Before any field is decrypted,
   `load` checks the on-disk marker is `"passphrase-v1"`. Mismatch → error.

A *third* implicit check sits at the value level: `Cipher::decrypt` rejects
any value whose prefix isn't `$WR;1.0;AES-256-GCM;`.

### `[passphrase]` block

```toml
[passphrase]
instance     = "mysite"
instance_sig = "<base64 HMAC-SHA256>"
salt          = "<base64 32-byte salt>"
```

- `instance` is the DNS-safe label chosen by the user at Setup (max 32
  chars, `[a-z0-9]([a-z0-9-]*[a-z0-9])?`). Used to construct the browser
  URL (`http://<instance>.resterm.localhost:<port>/auth/<key>`).
- `instance_sig` is `HMAC-SHA256(instance, derived_key)`, base64-encoded.
  Verified on Unlock to give a fast "wrong passphrase" error before
  attempting full config decryption.
- `salt` is the random 32-byte scrypt salt, base64-encoded. Generated once
  at Setup; the server uses the same salt on every Unlock so the derived
  key matches.

These fields live inline in `config.toml` so `config::peek` can read them
without decrypting anything — that resolves the chicken-and-egg between
"need the salt for the ceremony" and "need the ceremony to derive the
cipher."

## Atomic save

`config::save` writes `config.toml.tmp`, then renames it over `config.toml`.
POSIX rename is atomic within a filesystem, so a process killed mid-save leaves
the previous config intact rather than a truncated one; on Windows the same
call becomes `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`. The temp file is in
the same directory as the target so the rename can't cross filesystems.

The temp file is created with `mode 0600` on Unix. Windows has no chmod: a new
file inherits the ACL of its parent, and the config directory sits under
`%APPDATA%`, which already grants only the owner (plus SYSTEM and
Administrators). Either way this is mostly convention given the single-user
scope — the practical attacks the encryption is built against involve the file
leaving the device (cloud sync of a config dir, included in a backup, copied to
another machine) rather than another local account reading it in place. Disk
encryption is your responsibility.

## Config-directory lock

Only one resterm may hold a given config directory at a time. Startup takes an
exclusive lock on `<config-dir>/config.lock` via `std::fs::File::try_lock`
(`flock` on Unix, `LockFileEx` on Windows) and holds it until the process
exits; a second instance exits with an error instead of racing the first one's
saves. This protects against lost updates, not against an attacker — the lock
is advisory and anything that writes `config.toml` without taking it still
wins.

## Backing up `config.toml`

The config directory is `dirs::config_dir()` + `resterm`, so its location is
platform-dependent:

| OS | Config directory |
|---|---|
| Linux | `$XDG_CONFIG_HOME/resterm`, else `~/.config/resterm` |
| macOS | `~/Library/Application Support/resterm` |
| Windows | `%APPDATA%\resterm` (`C:\Users\<you>\AppData\Roaming\resterm`) |

**Copy `config.toml` and nothing else.** The directory also holds
`config.lock`, a zero-byte marker recreated on every launch, and
`config.toml.tmp`, which exists only for the instant between write and
rename.

```sh
# Linux
cp "${XDG_CONFIG_HOME:-$HOME/.config}/resterm/config.toml" ~/backups/

# macOS
cp "$HOME/Library/Application Support/resterm/config.toml" ~/backups/
```

```powershell
# Windows
Copy-Item "$env:APPDATA\resterm\config.toml" "$HOME\backups\"
```

To avoid the per-platform paths entirely, keep the config somewhere
path-stable and point resterm at it — the directory is created on first
use:

```sh
resterm --config-dir ~/Sync/resterm
```

Restoring is the reverse copy. Three things to know:

- **The file is self-contained.** The scrypt `salt` and `instance_sig` live
  inside `config.toml`, so the file plus your passphrase is everything
  needed to decrypt. There is no separate key material to lose.
- **The keychain entry is not in the file.** If you use keychain
  auto-unlock, that credential lives in the OS credential store. On a
  restored machine you enter the passphrase manually, then opt back into
  saving it to that machine's keychain.
- **There are no migrations.** `load` rejects any file whose `version` is
  not the one the binary expects, rather than upgrading it, so note which
  resterm version a backup came from.

Copying while resterm is running is safe to *read* — saves are atomic
(write temp, rename), so a copy sees either the old file or the new one,
never a half-written one. The lock file guards concurrent writers, not
readers.

## Is `config.toml` safe to back up unencrypted?

**Short answer:** the *secret* fields are safe — they're already AEAD-
encrypted in place. What leaks if the bare file is exposed is the
*shape* of your setup.

The TOML file always exposes, in plaintext:

- The profile names (`[profiles.foo]`, `[profiles.bar]`, …).
- The backend type per profile (`backend = "s3" | "rest" | "local"`).
- All public backend fields: `local_path`, `rest_url`, `s3_endpoint`,
  `s3_bucket`, `s3_region`, `s3_root`.
- The account identifiers `rest_user` and `s3_access_key` — see the table
  above for why these are deliberately not encrypted.
- Schema metadata: `version`, `cipher` marker.
- The `[passphrase]` block (`instance`, `instance_sig`,
  `salt`). The instance is a user-chosen label; the signature and salt are
  useless without the passphrase.

Everything else (repo passwords, `rest_password`, `s3_secret_key`) sits
behind `$WR;1.0;AES-256-GCM;` and is unreadable without the passphrase.

The key is never on disk; the `[passphrase]` block is metadata, not
material. An attacker with only `config.toml` would need to brute-force
the passphrase through resterm's scrypt parameters and then pass the
HMAC check.

### What's still in scope for a config-only leak

Even with the secret fields encrypted, an exfiltrated `config.toml`
tells an attacker:

- Which storage backends you use and where (S3 buckets, REST endpoints,
  local paths). That's a target list — they know where to look if they
  later obtain credentials.
- *Which account* those credentials would belong to, via `s3_access_key`
  and `rest_user`. Neither is a secret, but pairing an access key ID with
  its bucket and endpoint narrows the target further than the bucket name
  alone.
- Your profile count and naming conventions.

If that metadata is itself sensitive, treat `config.toml` like any
other secret file and encrypt the backup container.

## Key derivation

The 32-byte AEAD key is derived from the user's passphrase with
**scrypt**. The browser encrypts the passphrase to the localhost server
using the transport envelope described below; the Rust server then derives
the config key:

```
key = scrypt(passphrase, salt, log_n=16, r=8, p=1, len=32)
```

The salt is generated once at Setup (`random_bytes(32)`) and stored in
`[passphrase].salt` so subsequent unlocks regenerate the same key.

The passphrase is not sent in plaintext and is never written to disk. It
does exist in Rust process memory long enough for scrypt to run. The App
keeps only the derived 32-byte config key for the session.

### Instance signature

At Setup, the server computes `HMAC-SHA256(instance, derived_key)` after
scrypt finishes. This is stored in `[passphrase].instance_sig`.

On Unlock, the server re-computes the HMAC from the derived key and
compares (constant-time) against the stored signature. A mismatch means
the passphrase is wrong — the user gets a clear error immediately instead
of a cryptic AEAD tag failure on the first `$WR;1.0;AES-256-GCM;` value.

### Encrypt / decrypt

```
$WR;1.0;AES-256-GCM;<instance>;<base64( nonce(12) || ciphertext || tag(16) )>
```

The header fields are semicolon-delimited: app identifier, format
version, algorithm, and the passphrase instance name. The instance is
the same DNS-safe label from the `[passphrase]` block, making each
encrypted value self-documenting about which config it belongs to.

- 12-byte nonce from `OsRng` (aes-gcm's `generate_nonce`). Each
  encrypt mints a fresh nonce — there is no nonce reuse window even
  within a single save.
- AES-256-GCM with the 32-byte scrypt-derived config key.
- 16-byte GCM tag appended (the AEAD construction does this; the
  encrypted blob's last 16 bytes are the tag).

Decrypt strips the header and instance field, then requires
`len(payload) >= 28` (12 nonce + 16 tag), splits off the nonce, runs
`cipher.decrypt`, returns the UTF-8 plaintext. Tag failure = hard error.

### Why not whole-file passphrase encryption?

Per-value encryption lets the boot code read the non-secret
`[passphrase]` metadata before it has the key. It can then derive the key,
verify the instance signature, and decrypt only the encrypted profile fields.

## Passphrase input

Passphrases are entered in masked TUI fields.

**Setup** (`Screen::PassphraseSetup`): after the instance-name prompt, the
user enters and confirms a passphrase. It must be at least 12 characters and
include an uppercase letter, lowercase letter, digit, and special character.
scrypt runs synchronously on `Screen::PassphraseDerivingKey`, then resterm
computes the instance HMAC and saves the `[passphrase]` metadata. When
keychain support is enabled, a checkbox controls whether the passphrase is
stored there.

**Unlock**: without keychain support, resterm goes directly to the masked
manual input. With keychain support, `Screen::AuthMethodChoice` offers
`Use passphrase from keychain` and `Enter passphrase manually`. Manual
entry starts with keychain saving disabled but lets the user opt in. After
scrypt derives the key from the stored salt, `verify_instance_sig` checks
the HMAC. A mismatch returns to the manual prompt with an error.

The passphrase and derived key are never persisted in the config. Only the
salt, instance, and instance signature are stored there; the derived key
lives in application memory for the session.

## Share dialog signing key

The share server (`src/share.rs`) signs URLs with HMAC-SHA256 over
`(snap_id, tree_id, name, exp)`. The 32-byte signing key is derived via
`passphrase::derive_share_signing_key(config_key)` — SHA-256 over
`"resterm-share-v1\0" || config_key`.

Same key per identity (no per-session randomness) → share URLs stay valid
across resterm restarts within their 1-hour TTL. Different passphrase →
different key, so URLs minted under one passphrase cannot be replayed
against a server started under another.

This signing key is independent of `Cipher`: a tampered URL fails HMAC
verification before the server ever touches a `Cipher::decrypt`.

## Threat model and non-goals

resterm assumes a single-user, single-device deployment. The encryption
is sized for "the config file leaves the device" scenarios, not for
"there are hostile users on the same device" scenarios.

In scope:
- **Config exfiltration off-device.** A copy of `config.toml` that ends
  up in cloud sync, a generic file-share, a publicly readable backup,
  or a misconfigured snapshot does not by itself leak the secret
  fields. The attacker would need the passphrase (subject to scrypt
  brute-force resistance).
- **Mid-save corruption.** Atomic rename leaves the previous config
  intact even if the process is killed mid-write.

Not in scope:
- **Hostile local accounts on the same machine.** resterm doesn't
  defend against this — single-user scope.
- **Root / disk-image access.** Anyone with raw disk access or the
  passphrase can decrypt the config. Use full-disk
  encryption if that matters.
- **Memory disclosure** (core dumps, ptrace, swap). Cipher key bytes are
  ordinary heap memory — not `mlock`ed, not zeroized on drop.
- **Repo-level secrecy.** This is about `config.toml`, not about the
  restic repository itself. Restic has its own password (which resterm
  stores encrypted in the per-profile `password` field) and its own
  at-rest encryption.

For the surrounding system shape (boot flow, the share dialog, the runtime
loop), see [architecture.md](architecture.md).

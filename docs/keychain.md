# Keychain integration

resterm can optionally store the config passphrase in the OS keychain so
subsequent launches unlock automatically without prompting.

## Compile-time feature

Keychain support is behind the `keychain` Cargo feature and is **not enabled by
default**. The prebuilt macOS binary ships with it; the Linux binaries do not.

```sh
cargo build --release --features keychain
```

The feature uses `keyring-core` with platform-specific credential stores:
`apple-native-keyring-store` on macOS, `dbus-secret-service-keyring-store` on
Linux.

### Why not enabled on Linux by default

Linux is frequently used headless (servers, containers, SSH sessions) where no
keyring daemon is available. The D-Bus secret-service store requires:

- `libdbus-1-dev` at **build time**
- A running secret-service daemon at **runtime**

Neither is present on a typical headless host. Rather than shipping a dependency
that silently fails for most Linux deployments, the feature is opt-in.

To build with keychain on a Linux desktop:

```sh
sudo apt install libdbus-1-dev pkg-config   # build deps
cargo build --release --features keychain
```

A running GNOME Keyring or KDE Wallet is required at runtime.

## Runtime flag

Even when the binary is compiled with the `keychain` feature, you can disable
it at runtime:

```sh
resterm --no-keychain
```

This gives the same plain terminal passphrase flow as a binary built without
the feature — no keychain reads, no "save to keychain" checkbox. Useful for
simpler setup or environments where the keyring daemon is unavailable.

## How it works

When keychain is enabled:

- **Setup**: after entering and confirming the passphrase, a checkbox
  (`[x] Save passphrase to keychain`) lets the user opt in to storing it.
  The passphrase is saved under service `resterm` with the instance name as
  the account identifier.

- **Unlock**: resterm offers `Use passphrase from keychain` and
  `Enter passphrase manually`. The keychain option skips the input screen when
  a credential is found; otherwise it falls back to the manual TUI prompt.
  Choosing manual entry starts with the save-to-keychain checkbox cleared.

- **Wrong passphrase in keychain**: if the stored passphrase no longer matches,
  HMAC verification fails and the user is shown the manual entry screen with
  an error message.

## Supported backends

Platform credential stores (via `keyring-core`):

| Platform | Store crate | Backend |
|----------|-------------|---------|
| macOS    | `apple-native-keyring-store` | macOS Keychain (Security.framework) |
| Linux    | `dbus-secret-service-keyring-store` | D-Bus secret-service (GNOME Keyring / KDE Wallet) |

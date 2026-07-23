# wrustic

A terminal UI for browsing and managing [restic](https://restic.net/)-format
backup repositories, built on the restic 0.19 CLI and
[`ratatui`](https://crates.io/crates/ratatui).

`wrustic` is a read/write repository manager. Repository creation, backup,
restore, retention, and maintenance operations are in scope and are delegated
to restic subprocesses. The current UI exposes repository reads and snapshot
deletion; additional write workflows are tracked in
[`docs/roadmap.md`](docs/roadmap.md).

## Features

- **Backends**: local filesystem, REST-server, and S3
- **Profile management**: create, edit, and delete saved profiles; secrets
  (repository password, S3 keys) are encrypted per-value with AES-256-GCM
  under a passphrase-derived key
- **Snapshot browsing**: list snapshots, navigate the file tree, view file
  details, and compare two snapshots side-by-side
- **Snapshot filtering**: narrow by host, tag, or path
- **Snapshot deletion** via `restic forget`
- **File sharing**: one-time signed download URLs served from localhost
- **Keyboard and mouse navigation**: Vim-style keys, arrow keys, PgUp/PgDn,
  mouse click/scroll; `--no-mouse` to disable
- **Passphrase entry**: masked TUI input, with optional keychain auto-unlock
- **Keychain integration** (macOS): optionally save the passphrase to the OS
  keychain for auto-unlock; see [`docs/keychain.md`](docs/keychain.md)

See [`docs/roadmap.md`](docs/roadmap.md) for planned features.

## Install (prebuilt binary)

A convenience script downloads the latest release binary from GitHub and
drops it at `$HOME/.local/bin/wrustic` — no `sudo`, no system-wide install.
Supported targets: `linux-amd64`, `linux-arm64`, `macos-arm64`.

```sh
curl -fsSL https://raw.githubusercontent.com/andrewtheguy/wrustic/main/install.sh | bash
```

Or clone the repo and run `./install.sh` directly. Useful flags:

- `./install.sh v0.0.1` — install a specific release tag
- `./install.sh --prerelease` — grab the latest prerelease
- `./install.sh --download-only` — drop the binary in the current directory
- `RELEASE_TAG=v0.0.1 ./install.sh` — same as passing the tag positionally

The script verifies the SHA-256 of the downloaded binary against the digest
GitHub publishes in the release metadata before installing, and runs the
binary's `--help` once to confirm it loads on the host. If `$HOME/.local/bin`
is not on your `$PATH`, the script prints the line you need to add to your
shell profile.

## Build & run

Requires a Rust toolchain (developed against rustc 1.93).

Platform: Linux / macOS only.

```sh
cargo run
```

### CLI flags

```text
wrustic [-c|--config-dir <PATH>] [-p|--port <N>] [--no-keychain] [-h|--help]
```

`--config-dir <PATH>` overrides the default config location
(`~/.config/wrustic`). Useful for keeping separate profile sets, running
tests, or driving an automation/CI flow against a throwaway directory:

```sh
cargo run -- --config-dir ./tmp/wrustic-sandbox
```

The directory is created on first run if it doesn't exist.

`--port <N>` selects the localhost port for the file-share dialog
(default: 7834).

`--no-keychain` disables keychain integration at runtime, even when the
binary was built with the `keychain` feature. See
[`docs/keychain.md`](docs/keychain.md) for details on keychain support,
why it is not enabled on Linux by default, and how to build with it.

### First run

On first run (no existing `config.toml`), wrustic prompts for an
**instance name** — a short DNS-safe label (e.g. `laptop`, `workstation`).
Then you set a passphrase (min 12 chars, must include uppercase, lowercase,
digit, and special character). The passphrase derives a 32-byte encryption
key via scrypt; this key encrypts all secret fields in `config.toml`.

On subsequent launches, you re-enter the passphrase to unlock.

Then in the TUI:
1. The main menu lists saved profiles. Press `n` to create a new one.
2. **Create new profile**: type a profile name, pick a
   backend (Local / REST / S3), fill in the per-backend prompts (Esc on any
   prompt goes back one step):
   - **Local**: filesystem path, e.g. `./tmp/repo`.
   - **REST**: URL, e.g. `http://localhost:8000/` or `https://user:pass@host/path/`.
   - **S3**: endpoint (blank -> AWS default), bucket, region (blank -> `us-east-1`),
     access key ID, secret access key (masked).
   Finally type the repository password (masked) and press Enter — the profile
   is encrypted into `config.toml` and you return to the main menu.
3. **Open a profile**: pick a profile from the list, and the snapshot view
   opens directly. The repo password (stored encrypted in the profile) is
   applied automatically — the passphrase you entered at launch is the only
   credential you type.
4. **Delete a profile**: press `d` on a profile and confirm with `y`.

## Relationship to the `restic` binary

`wrustic` requires **restic >= 0.19.1** on `PATH`. Repository reads and the
exposed `forget` operation run as short-lived restic subprocesses. Structured
operations use restic's JSON/JSONL output; downloads stream `restic dump`
stdout directly to the localhost HTTP response.

The repository password is never placed in an environment variable or command
argument. Wrustic launches restic with `--password-file /dev/stdin`, writes the
password through the child's anonymous stdin pipe, and closes the pipe before
reading output.

All dev/test artifacts in the snippets below go under the project's `./tmp/`
directory (already in `.gitignore`) rather than the system `/tmp` — this keeps
the workspace self-contained and sidesteps permission issues.

### Garage S3 end-to-end test

The Garage server and the wrustic integration test have separate runners.
Start a fresh server in its own terminal:

```sh
./scripts/garage-test-server.sh --reset
```

The server script downloads the pinned Garage binary into `./tmp/tools/` when
needed and directly runs `server --single-node --default-bucket` in the
foreground. Only its S3 API is configured; the web and admin APIs are omitted.
It does not run restic, Cargo, or tests, and Ctrl-C stops it. Omit `--reset` to
reuse the existing Garage data.

With a fresh server running, seed the repository and run the integration test
independently:

```sh
./scripts/garage-e2e.sh run
```

`seed` and `test` are also available separately. Test state stays under
`./tmp/garage-e2e/`. The E2E runner requires restic >= 0.19.1 from
`./tmp/tools/restic` or `PATH`; use `RESTIC_BIN=/path/to/restic` to select
another binary. The repository password is piped to
`--password-file /dev/stdin` and is never exported.

The S3 and internal RPC ports default to 3900 and 3901. Select different ports
with `GARAGE_S3_PORT` and `GARAGE_RPC_PORT`; only the S3 value is needed by the
E2E client:

```sh
GARAGE_S3_PORT=3910 GARAGE_RPC_PORT=3911 ./scripts/garage-test-server.sh --reset
GARAGE_S3_PORT=3910 ./scripts/garage-e2e.sh run
```

Typical dev loop to get something to point `wrustic` at:

```sh
export RESTIC_PASSWORD=test
export RESTIC_REPOSITORY=./tmp/test-repo
restic init
echo hello > ./tmp/test-file
restic backup ./tmp/test-file
restic backup --tag demo ./tmp/test-file

cargo run   # pick "Local filesystem", enter ./tmp/test-repo, then password
```

### REST-server dev workflow

`wrustic` reaches a [restic REST
server](https://github.com/restic/rest-server) through the restic CLI.
`rest-server` is the easiest local peer for exercising that backend.

Fetch and run [rest-server v0.14.0](https://github.com/restic/rest-server/releases/tag/v0.14.0)
(substitute the asset for your platform):

```sh
cd ./tmp
curl -fsSLO https://github.com/restic/rest-server/releases/download/v0.14.0/rest-server_0.14.0_linux_amd64.tar.gz
echo "4c9c95bc079a0334e81fad379b19dc5c3353c71c2c88d652cafce2081c2b1c66  rest-server_0.14.0_linux_amd64.tar.gz" | sha256sum -c
tar -xzf rest-server_0.14.0_linux_amd64.tar.gz
chmod +x rest-server_0.14.0_linux_amd64/rest-server   # the tarball ships mode 0644

mkdir -p ./rest-data
./rest-server_0.14.0_linux_amd64/rest-server \
    --listen :8000 --path ./rest-data --no-auth &
cd ..
```

Then seed a repo through it with `restic`, and point `wrustic` at the same URL:

```sh
export RESTIC_REPOSITORY=rest:http://localhost:8000/
export RESTIC_PASSWORD=test
restic init
echo hello > ./tmp/test-file
restic backup ./tmp/test-file
restic backup --tag demo ./tmp/test-file

cargo run   # pick "REST server", enter http://localhost:8000/, then password
```

Note: `--no-auth` is a local-only convenience. For anything outside a dev
machine, use `--htpasswd-file` and TLS per the `rest-server` documentation;
`wrustic` accepts credentials embedded in the URL (`https://user:pass@host/`).

### S3 dev workflow (via `rclone serve s3`)

`wrustic` talks S3 through restic — there's no built-in dev S3 server. The
simplest stand-in is [`rclone serve s3`](https://rclone.org/commands/rclone_serve_s3/)
pointed at a local directory, which lets you exercise the full S3 code path
without an AWS account.

Requires `rclone` (>= v1.73) on `$PATH`. `serve s3` is marked **Experimental**
upstream but is sufficient for dev.

```sh
mkdir -p ./tmp/s3-data/wrustic-bucket    # bucket dir must pre-exist

rclone serve s3 ./tmp/s3-data \
    --addr 127.0.0.1:8333 \
    --auth-key 'wrustic-key,wrustic-secret' \
    --force-path-style=true \
    >./tmp/rclone-s3.log 2>&1 &
```

Seed a repo through it with `restic` (uses standard AWS env vars; the region
value is meaningless to rclone but restic's SDK requires *some* region):

```sh
export AWS_ACCESS_KEY_ID=wrustic-key
export AWS_SECRET_ACCESS_KEY=wrustic-secret
export AWS_DEFAULT_REGION=us-east-1
export RESTIC_REPOSITORY=s3:http://127.0.0.1:8333/wrustic-bucket
export RESTIC_PASSWORD=test
restic init
echo hello > ./tmp/test-file
restic backup ./tmp/test-file
restic backup --tag demo ./tmp/test-file
```

Then in `wrustic`, pick **S3** and enter:

| Prompt        | Value                       |
| ------------- | --------------------------- |
| endpoint      | `http://127.0.0.1:8333`     |
| bucket        | `wrustic-bucket`            |
| region        | `us-east-1` (or leave blank)|
| access key ID | `wrustic-key`               |
| secret access | `wrustic-secret`            |
| password      | `test`                      |

Cleanup: `kill` the `rclone` process and `rm -rf ./tmp/s3-data`.

Caveats:
- `rclone serve s3` does **not** auto-create buckets; the bucket directory has
  to exist on disk before `restic init`.
- `--force-path-style=true` is required — rclone's S3 server doesn't speak
  virtual-hosted-style addressing.
- Profiles are persisted in `~/.config/wrustic/config.toml`. Secret fields
  such as the restic password and S3 keys are encrypted per value with
  AES-256-GCM under a passphrase-derived key. The file itself is not a
  whole-file encrypted archive; see `docs/encryption.md` for the on-disk schema
  and threat model.

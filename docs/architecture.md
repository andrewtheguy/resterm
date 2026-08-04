# Architecture

A working map of resterm for anyone reading the code. Reflects current state
on `main`; if a section disagrees with the source, the source wins — please
update this file.

## What resterm is

A terminal UI for browsing and managing restic backup repositories. It opens
a repo, lists snapshots, lets you walk the file tree, inspect file details,
diff two snapshots, and download a single file via a localhost signed URL.
The product scope includes repository writes such as initialization, backup,
restore, retention, and maintenance. Snapshot deletion is the first write
workflow currently exposed by the UI.

**Scope: single-user, single-device.** resterm is a personal tool — one
person, one machine (or one account on a shared box that they fully own).
Multi-user, multi-tenant, and shared-host scenarios are explicitly out of
scope: no per-user config separation, no privilege boundary inside the
binary, no defense-in-depth against another local account on the same
machine. This shapes the on-disk permissions, the threat model in
[encryption.md](encryption.md), and the choice to keep all state in one
flat `App` struct.

It does not reimplement restic's repository engine:
- Repository operations go through restic >= 0.19 subprocesses.
- Structured results use restic JSON/JSONL output, while file content and
  progress-oriented operations are streamed over pipes.
- Read and write workflows share the same subprocess boundary and password
  transport guarantees.

## Runtime shape

```
main()
 └── App::boot(config_dir, port, no_keychain)  // app.rs
      ├── config::paths(override) → Paths { config }
      ├── start_passphrase_flow()           // app.rs + passphrase.rs
      └── load_config_or_set_fatal()
            └── config::load(paths, cipher) → Config (with profiles decrypted)
 │
 └── while !app.quit { terminal.draw(render); dispatch }
     ├── async-ish screens (Loading, Verifying, OpeningSnapshot,
     │   LoadingDir, LoadingFileDetails, SnapshotDeleteContentsLoading,
     │   SnapshotDeleting, SnapshotCompareLoading) — main.rs runs the
     │   blocking work synchronously and transitions the screen
     ├── Screen::PassphraseDerivingKey — runs scrypt synchronously
     └── otherwise — blocking event::read(), App::handle_key/mouse
```

The event loop lives in `main.rs` rather than `App` because some screens need
to take long-blocking work out of the rendering tick. Each "async-ish" branch
matches a `Screen::*Loading` variant, runs the blocking call inline, and
transitions to the next screen — there is no real async/await in the main
loop. The share server is the only true async machinery, isolated on its own
OS thread + tokio current-thread runtime.

## State: `App` and `Screen`

`Screen` (in `app.rs`) is the discriminator for what's on screen. It's
about three dozen variants spanning first-run, profile CRUD, snapshot list,
snapshot delete/compare flows, tree browsing, file details, share dialog,
and passphrase dialog. Every screen is rendered by a corresponding
`render_<screen>` function in `ui.rs`.

`App` (in `app.rs`) is a flat struct holding *every* piece of session state.
This is deliberate — resterm is small enough that a fat struct + an enum
discriminator is more legible than a nested per-screen state machine. The
struct includes:
- Always-present: `screen`, `paths`, `config`, `cipher`, `server_port`.
- Profile-creation scratch (`new_profile_name`, `local_path`, …) cleared
  whenever `enter_home()` runs.
- Snapshot browse state: `snapshots`, `repo_session`, `browse_stack`,
  `pending_descend` / `pending_file_lookup` / `pending_refresh_path`.
- Share dialog: `share_target`, `share_handle`, `share_url`, etc.
- Passphrase dialog: `passphrase_input`, `passphrase_confirm`,
  `passphrase_instance_input`, `passphrase_phase`, `passphrase_error`.

Keypress handling is concentrated in `App::handle_key` (single big match on
`self.screen`); mouse in `App::handle_mouse`.

## Config + crypto

`src/config.rs` owns the TOML schema (`Config`, `Profile`,
`PassphraseMeta`) and the atomic save: write `config.toml.tmp` (mode 0600 on
Unix; on Windows it inherits the per-user ACL of `%APPDATA%`), then rename over
the target — `rename(2)` on Unix, `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`
on Windows.

`config::acquire_lock` takes an exclusive lock on `<config-dir>/config.lock` at
startup and holds it for the process lifetime, stored on `App` as
`config_lock`. resterm keeps the whole config in memory and rewrites the file
wholesale on every save, so a second instance on the same directory would
silently drop the first one's profiles; it is refused before the TUI starts
instead. The lock is `std::fs::File::try_lock` (stable since Rust 1.89) —
`flock(LOCK_EX | LOCK_NB)` on Unix, `LockFileEx` on Windows. No third-party
crate, and no stale-lock cleanup, because the kernel releases it when the
process dies for any reason.

Encryption is per-value (not whole-file) so non-secret edits diff cleanly.
For schema details, key derivation, threat model, and
the share-server signing-key derivation, see
[encryption.md](encryption.md).

## Localhost server (`share.rs`)

- One OS thread and one `tokio::runtime::Builder::new_current_thread`.
  No global runtime or shared executor.
- Binds on `127.0.0.1:<port>` and `[::1]:<port>`. User-facing URLs use
  `localhost`.
- Returns a `ShareHandle` that owns a
  `oneshot::Sender<()>` for shutdown plus a `JoinHandle`. Drop = stop server.
  Explicit `.stop()` joins the thread (port released by the time it returns).
- Routes are spelled out as a flat `match` inside one `async fn handle()`;
  there is no router crate.

### Share dialog (`src/share.rs`)

- Per-file: each `start()` call is bound to one `(snap_id, tree_id, name)`.
  A URL minted for file A cannot be replayed against a later server bound to
  file B — the name is part of the HMAC.
- HMAC signing key is derived from the passphrase-derived config key (via
  `passphrase::derive_share_signing_key`). Same key per passphrase → URLs
  survive across restarts within the TTL.
- Routes:
  - `GET /dl?snap=…&tree=…&exp=…&sig=…` — verifies sig + expiry, streams the
    file.
  - `GET /s/<short_id>` — 302 to the long `/dl?…` URL. `short_id` is a
    16-hex-char random alias generated at `start()`.
  - Anything else → 404.
- TTL: `SHARE_TTL = 1 h` baked into the signed `exp` claim. The server
  enforces expiry independently of any wall-clock state on its end.

### Passphrase (`src/passphrase.rs`)

No server is started. `passphrase.rs` exposes
`derive_config_key`, `verify_instance_sig`, `compute_instance_sig`, and
`passphrase_policy_error` for direct use by `app.rs`. Key derivation runs
synchronously on `Screen::PassphraseDerivingKey`.

## Repository access (`src/repo.rs`, `src/restic.rs`)

`restic.rs` owns subprocess construction and credential transport:
- `detect()` requires restic >= 0.19, and reads the version from
  `restic version --json` rather than parsing the prose line.
- Two restic global flags apply throughout. A cache flag goes on every
  invocation, added centrally by `apply_cache_flag()` from `command()` (and from
  `detect()`): `--no-cache` by default, or `--cache-dir` pointed at
  `dirs::cache_dir()/resterm` when the `--cache` CLI flag set the
  `CACHE_ENABLED` static at startup. Either spelling keeps resterm from sharing
  restic's on-disk cache with other CLI instances; the per-user cache root is
  what keeps two users' caches apart, without a uid in the path and without a
  `#[cfg(unix)]` branch for a concept Windows lacks. If no cache root can be
  named, caching stays off rather than falling back to restic's default. `--json`
  goes on every invocation that supports it, added by each caller next to its
  subcommand; `dump` is the sole exception, since its stdout is the file's raw
  bytes.
- `command()` also removes inherited restic password variables and configures
  the repository/backend.
- The repository password is written to an anonymous pipe on the child's stdin;
  it never appears in argv or the environment. On Unix restic is pointed at it
  explicitly with `--password-file /dev/stdin`. Windows has no such path, and
  needs none: restic reads the password straight off stdin whenever stdin is
  not a terminal, which is always the case here because `command()` pipes it.
- `run()` captures structured command output.
- `stream_dump()` streams file bytes with backpressure and kills the child if
  the HTTP client disconnects.
- `forget()` performs the repository mutation currently exposed by the UI.
- `unlock()` runs `restic unlock --json` when `is_lock_error()` identifies a
  lock failure during snapshot deletion. The TUI offers this with `u`, then
  rebuilds the delete confirmation and retries the flow after stale locks are
  removed.
  Future write workflows belong beside it and must retain the same credential
  and structured-output boundaries.

`repo.rs` translates restic JSON into UI models:
- `snapshots --json` lists snapshots.
- `cat snapshot` and `cat tree snapshot:path` preserve tree IDs, content
  hashes, ownership, link targets, and timestamps.
- A per-browse `RepoSession` maps tree IDs to restic snapshot-path selectors.
- `diff --json` supplies JSONL changes and statistics.
- The snapshot-delete preview walks the top of a snapshot breadth-first with
  `ls --json <snapshot> <dir…>`. Positional arguments are directory filters and
  `--recursive` is not passed, so restic decodes only the named directories:
  one invocation per level, and a cost that does not grow with the size of the
  snapshot. The walk starts at the snapshot's own `paths` where those are
  absolute — a backup of `/home/andrew/projects` otherwise spends an
  invocation per ancestor directory before reaching anything worth showing —
  and falls back to the tree root when they are not usable as filters, which
  is the case for Windows `C:\…` paths and for backups taken from a relative
  path. Listing a snapshot recursively instead would make restic fetch every
  tree in it, which is a network round trip per directory on a remote backend
  when no cache is kept — the default.

The share server invokes `restic dump <snapshot> <path>` for each accepted
download and forwards stdout through a bounded channel to Hyper.

## Verification and dev flow

Run from `AGENTS.md`:
- `cargo clippy --all-features` and `cargo test --all-features` after every
  Rust change. Don't run `cargo fmt` — it churns the diff.
- For local testing, use `cargo run -- --config-dir ./tmp/resterm-sandbox`
  so the production config dir (`~/.config/resterm`, or `%APPDATA%\resterm`
  on Windows) is never touched.
- Test fixtures live under `./tmp/` (gitignored).
- Live integration fixtures create repositories and sources under `./tmp/`
  and use restic for all repository mutations.

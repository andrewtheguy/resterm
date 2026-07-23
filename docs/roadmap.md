# Roadmap

## Repository writes

- Initialize a repository from a new or existing profile
- Back up selected source paths with tags, excludes, host selection, dry-run,
  and streamed progress
- Restore a snapshot or selected paths to a chosen destination
- Manage retention policies with `forget --keep-*` previews and explicit
  confirmation
- Reclaim repository storage with `prune`, including progress and cancellation
- Run repository integrity checks and surface structured results
- Manage repository keys and password rotation without exposing secrets in
  environment variables or command arguments

All write operations continue to run through restic >= 0.19.1. Repository
passwords must use the anonymous stdin pipe with
`--password-file /dev/stdin`; progress and results should use restic's
structured output wherever available.

## Browsing and platform

- Read passphrase from stdin (for securely passing passwords from external tools like password managers or keychain backends not supported by this program)
- Directory-level diff: compare arbitrary subdirectories across arbitrary snapshots
  — e.g. compare `/photos` in one snapshot against `/backup/old-photos`
  in another — useful for backups from different machines or after a
  directory has been moved
- Moved-file detection in diffs (identify files that were renamed or
  relocated rather than showing them as a delete + add)
- Search across snapshot contents (find files by name or path pattern)
- Search text file contents across snapshots (requires indexing and is a much larger project)
- Windows support (needs build and runtime testing on Windows)

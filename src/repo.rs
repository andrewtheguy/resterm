use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::config::Profile;
use crate::restic;

pub(crate) struct SnapshotRow {
    pub(crate) id: String,
    pub(crate) time: String,
    pub(crate) host: String,
    pub(crate) tags: Vec<String>,
    pub(crate) paths: Vec<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum ContentKind {
    Parent,
    Dir,
    File,
    Symlink,
    Other,
}

impl ContentRow {
    pub(crate) fn parent() -> Self {
        Self {
            name: "..".to_string(),
            kind: ContentKind::Parent,
            size: 0,
            mtime: String::new(),
            subtree: None,
        }
    }
}

pub(crate) struct ContentRow {
    pub(crate) name: String,
    pub(crate) kind: ContentKind,
    pub(crate) size: u64,
    pub(crate) mtime: String,
    pub(crate) subtree: Option<String>,
}

#[derive(Clone)]
pub(crate) struct PreviewEntry {
    pub(crate) path: String,
    pub(crate) kind: ContentKind,
    pub(crate) size: u64,
}

#[derive(Clone)]
pub(crate) struct ContentsPreview {
    pub(crate) entries: Vec<PreviewEntry>,
    pub(crate) truncated: bool,
}

pub(crate) struct DeleteSnapshotInfo {
    pub(crate) hostname: String,
    pub(crate) paths: Vec<String>,
    pub(crate) tags: Vec<String>,
}

pub(crate) struct FileDetails {
    pub(crate) name: String,
    pub(crate) full_path: String,
    pub(crate) kind: ContentKind,
    pub(crate) kind_label: String,
    pub(crate) size: u64,
    pub(crate) mode: Option<u32>,
    pub(crate) mtime: Option<String>,
    pub(crate) atime: Option<String>,
    pub(crate) ctime: Option<String>,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) linktarget: Option<String>,
    pub(crate) content_hashes: Vec<String>,
}

/// Parsed restic tree objects, keyed by tree id.
///
/// A restic tree id is the hash of the tree object's plaintext, so an entry can
/// never go stale and never has to be invalidated: the same id always names the
/// same directory contents, in this repository or any other. That is also what
/// makes the cache worth keeping across snapshots — an incremental backup reuses
/// the tree of every directory that did not change, so browsing a second
/// snapshot of the same source reads most of its directories out of here instead
/// of off the backend, which on a remote repository is a round trip apiece.
///
/// Held by `App` rather than by [`RepoSession`], which is dropped and rebuilt
/// every time a snapshot is opened.
#[derive(Clone, Default)]
pub(crate) struct TreeCache {
    trees: Arc<Mutex<HashMap<String, Arc<TreeDocument>>>>,
}

/// Bound on the cache, so that walking a very large tree cannot grow it without
/// limit. Past this, further trees are simply not stored: the ones already held
/// are those nearest the directories opened first, which are also the ones
/// walked back through most often.
const TREE_CACHE_MAX_TREES: usize = 4096;

impl TreeCache {
    fn lock(&self) -> Result<MutexGuard<'_, HashMap<String, Arc<TreeDocument>>>> {
        self.trees
            .lock()
            .map_err(|_| anyhow!("restic tree cache was poisoned"))
    }

    fn get(&self, tree_id: &str) -> Result<Option<Arc<TreeDocument>>> {
        Ok(self.lock()?.get(tree_id).cloned())
    }

    fn insert(&self, tree_id: &str, tree: Arc<TreeDocument>) -> Result<()> {
        let mut trees = self.lock()?;
        if trees.len() < TREE_CACHE_MAX_TREES {
            trees.insert(tree_id.to_string(), tree);
        }
        Ok(())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().expect("tree cache").len()
    }
}

#[derive(Clone)]
pub(crate) struct RepoSession {
    profile: Profile,
    /// Maps a tree id to the `<snapshot>:<path>` selector restic needs to fetch
    /// it. Unlike [`TreeCache`] this is per-session: a selector names a path in
    /// one snapshot, so it is rebuilt as each snapshot is walked.
    tree_selectors: Arc<Mutex<HashMap<String, String>>>,
    trees: TreeCache,
}

impl RepoSession {
    fn selector_for(&self, tree_id: &str) -> Result<String> {
        self.tree_selectors
            .lock()
            .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
            .get(tree_id)
            .cloned()
            .ok_or_else(|| anyhow!("no restic snapshot path registered for tree `{tree_id}`"))
    }

    fn register_selector(&self, tree_id: String, selector: String) -> Result<()> {
        self.tree_selectors
            .lock()
            .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
            .insert(tree_id, selector);
        Ok(())
    }
}

#[derive(Deserialize)]
struct ResticSnapshot {
    id: String,
    time: Option<String>,
    hostname: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct TreeDocument {
    nodes: Vec<TreeNode>,
}

#[derive(Clone, Deserialize)]
struct TreeNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    mode: Option<u32>,
    mtime: Option<String>,
    atime: Option<String>,
    ctime: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    user: Option<String>,
    group: Option<String>,
    #[serde(default)]
    size: u64,
    linktarget: Option<String>,
    content: Option<Vec<String>>,
    subtree: Option<String>,
}

fn parse_tree_id(value: &str) -> Result<String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(value.to_string())
    } else {
        bail!("invalid restic tree id `{value}`")
    }
}

fn display_time(value: Option<String>) -> String {
    value
        .and_then(|time| time.get(..19).map(|short| short.replace('T', " ")))
        .unwrap_or_default()
}

fn node_kind(kind: &str) -> ContentKind {
    match kind {
        "dir" => ContentKind::Dir,
        "file" => ContentKind::File,
        "symlink" => ContentKind::Symlink,
        _ => ContentKind::Other,
    }
}

pub(crate) fn verify_profile(profile: &Profile) -> Result<()> {
    restic::run(profile, &["cat", "config", "--json"])?;
    Ok(())
}

pub(crate) fn load_snapshots(profile: &Profile) -> Result<Vec<SnapshotRow>> {
    let output = restic::run(profile, &["snapshots", "--json"])?;
    let mut snapshots: Vec<ResticSnapshot> =
        serde_json::from_slice(&output).context("parsing restic snapshots JSON")?;
    snapshots.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(snapshots
        .into_iter()
        .map(|snapshot| SnapshotRow {
            id: snapshot.id,
            time: display_time(snapshot.time),
            host: snapshot.hostname.unwrap_or_default(),
            tags: snapshot.tags,
            paths: snapshot.paths,
        })
        .collect())
}

/// Open a browsing session. `trees` is shared with every other session opened
/// from the same [`TreeCache`], which is how a directory read while browsing one
/// snapshot is still in hand when the next snapshot reaches the same tree id.
pub(crate) fn open_indexed(profile: &Profile, trees: TreeCache) -> Result<RepoSession> {
    Ok(RepoSession {
        profile: profile.clone(),
        tree_selectors: Arc::new(Mutex::new(HashMap::new())),
        trees,
    })
}

fn load_tree(repo: &RepoSession, tree_id: &str) -> Result<Arc<TreeDocument>> {
    load_tree_with(repo, tree_id, |selector| {
        restic::run(&repo.profile, &["cat", "tree", selector, "--json"])
    })
}

/// The cache check around one `restic cat tree`, with the fetch injected so the
/// caching itself can be tested without a repository.
///
/// A hit needs no selector at all, which matters beyond saving the lookup: the
/// tree may have been read under a different snapshot entirely.
fn load_tree_with(
    repo: &RepoSession,
    tree_id: &str,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>>,
) -> Result<Arc<TreeDocument>> {
    if let Some(tree) = repo.trees.get(tree_id)? {
        return Ok(tree);
    }
    let selector = repo.selector_for(tree_id)?;
    let output = fetch(&selector)?;
    let tree: TreeDocument = serde_json::from_slice(&output)
        .with_context(|| format!("parsing restic tree JSON for `{selector}`"))?;
    let tree = Arc::new(tree);
    repo.trees.insert(tree_id, Arc::clone(&tree))?;
    Ok(tree)
}

pub(crate) fn snapshot_root_tree(repo: &RepoSession, snapshot_id: &str) -> Result<String> {
    let (snapshot, _) = restic::snapshot_details_json(&repo.profile, snapshot_id)?;
    let tree = snapshot
        .tree
        .ok_or_else(|| anyhow!("snapshot `{snapshot_id}` has no root tree"))?;
    let tree_id = parse_tree_id(&tree)?;
    repo.register_selector(tree_id.clone(), format!("{snapshot_id}:/"))?;
    Ok(tree_id)
}

pub(crate) fn list_tree(repo: &RepoSession, tree_id: &str) -> Result<Vec<ContentRow>> {
    // Read before the tree, and on a cache hit as much as on a miss: a child's
    // selector names a path in *this* snapshot, so it has to be registered even
    // when the tree itself came out of the cache under another snapshot.
    let parent_selector = repo.selector_for(tree_id)?;
    let mut rows = load_tree(repo, tree_id)?
        .nodes
        .iter()
        .map(|node| {
            let subtree = node.subtree.as_deref().map(parse_tree_id).transpose()?;
            if let Some(ref subtree) = subtree {
                let (snapshot, path) = parent_selector
                    .split_once(':')
                    .ok_or_else(|| anyhow!("invalid restic tree selector `{parent_selector}`"))?;
                let child_path = if path == "/" {
                    format!("/{}", node.name)
                } else {
                    format!("{}/{}", path.trim_end_matches('/'), node.name)
                };
                repo.register_selector(subtree.clone(), format!("{snapshot}:{child_path}"))?;
            }
            Ok(ContentRow {
                name: node.name.clone(),
                kind: node_kind(&node.kind),
                size: node.size,
                mtime: display_time(node.mtime.clone()),
                subtree,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|a, b| {
        let ad = matches!(a.kind, ContentKind::Dir);
        let bd = matches!(b.kind, ContentKind::Dir);
        bd.cmp(&ad).then_with(|| a.name.cmp(&b.name))
    });
    Ok(rows)
}

pub(crate) fn get_file_details(
    repo: &RepoSession,
    tree_id: &str,
    file_name: &str,
    full_path: String,
) -> Result<FileDetails> {
    let node = load_tree(repo, tree_id)?
        .nodes
        .iter()
        .find(|node| node.name == file_name)
        .cloned()
        .ok_or_else(|| anyhow!("file `{file_name}` not found in tree"))?;
    let (kind, kind_label) = match node.kind.as_str() {
        "file" => (ContentKind::File, "file".to_string()),
        "dir" => (ContentKind::Dir, "directory".to_string()),
        "symlink" => (
            ContentKind::Symlink,
            format!(
                "symlink → {}",
                node.linktarget.as_deref().unwrap_or("(missing target)")
            ),
        ),
        other => (ContentKind::Other, other.to_string()),
    };
    Ok(FileDetails {
        name: file_name.to_string(),
        full_path,
        kind,
        kind_label,
        size: node.size,
        mode: node.mode,
        mtime: node.mtime,
        atime: node.atime,
        ctime: node.ctime,
        uid: node.uid,
        gid: node.gid,
        user: node.user,
        group: node.group,
        linktarget: node.linktarget,
        content_hashes: node.content.unwrap_or_default(),
    })
}

#[derive(Deserialize)]
struct LsNode {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
}

/// The path restic uses for a snapshot's root when filtering `ls`.
const PREVIEW_ROOT: &str = "/";

/// Directories expanded per level. One level is one restic invocation whose
/// argv carries every directory in it, so this also keeps the command line far
/// below the ~32 KB Windows limit even for long paths.
const PREVIEW_MAX_DIRS_PER_LEVEL: usize = 64;

/// Entries taken from one directory before the walk moves on to its siblings.
const PREVIEW_ENTRIES_PER_DIR: usize = 10;

/// Ceiling on restic invocations for one preview, and so on how long it can
/// take. What normally stops the walk is the entry budget, not this: a level
/// that yields plenty of entries fills `limit` within two or three calls. The
/// cap only bites on a snapshot whose top is a long chain of near-empty
/// directories — which happens whenever the backup source was a deeply nested
/// path, since restic's tree mirrors it, e.g. `/home/andrew/projects/thing`.
/// Such levels cost one entry each, so the base has to clear a realistic path
/// depth; the `limit / 50` term then hands each press of "load more" another
/// level, so paginating past the cap still reveals something new.
fn preview_max_levels(limit: usize) -> usize {
    8 + limit / 50
}

/// Walk the top of a snapshot breadth-first, one restic invocation per level,
/// stopping as soon as `limit` entries are collected.
///
/// Cost is bounded by the number of levels walked, *not* by the size of the
/// snapshot — a 50-entry preview of a million-file backup costs the same one
/// or two restic calls as a preview of a tiny one.
///
/// `snapshot_paths` is the snapshot's own `paths` field, used to start the walk
/// at the directories that were backed up. See [`preview_roots`].
pub(crate) fn preview_snapshot_contents(
    repo: &RepoSession,
    snapshot_id: &str,
    snapshot_paths: &[String],
    limit: usize,
) -> Result<ContentsPreview> {
    preview_from_paths(snapshot_paths, limit, |dirs| {
        let output = restic::ls_children_json(&repo.profile, snapshot_id, dirs)?;
        let text = String::from_utf8(output).context("restic ls output was not UTF-8")?;
        parse_ls_children(&text, dirs)
    })
}

/// Seed the walk at the snapshot's own paths and fall back to the tree root if
/// they turn out to list nothing. Separated from restic so the fallback — a
/// correctness path, not an optimisation — can be tested against a synthetic
/// tree.
fn preview_from_paths(
    snapshot_paths: &[String],
    limit: usize,
    mut fetch: impl FnMut(&[String]) -> Result<Vec<(String, Vec<LsNode>)>>,
) -> Result<ContentsPreview> {
    let roots = preview_roots(snapshot_paths);
    let preview = build_preview(&roots, limit, &mut fetch)?;
    // A snapshot taken from a *relative* path records absolutised `paths` that
    // its tree does not mirror (restic stores `/deep/src` for `backup
    // deep/src`), so the seeded roots match nothing. Falling back to the tree
    // root costs one extra call in that case and keeps the preview correct.
    if preview.entries.is_empty() && roots != [PREVIEW_ROOT] {
        return build_preview(&[PREVIEW_ROOT.to_string()], limit, &mut fetch);
    }
    Ok(preview)
}

/// Where to start the walk.
///
/// A snapshot's tree mirrors the path that was backed up, so a backup of
/// `/home/andrew/projects` buries everything interesting under a chain of
/// single-child directories — `/`, then `/home`, then `/home/andrew` — each of
/// which would cost a restic invocation to step through and tells the reader
/// nothing. Starting at the snapshot's own paths skips the chain entirely and
/// spends the first call on actual content.
///
/// Only absolute paths are usable as `restic ls` filters; anything else (a
/// Windows `C:\…` path, say) falls back to the tree root.
fn preview_roots(snapshot_paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let roots: Vec<String> = snapshot_paths
        .iter()
        .filter(|path| path.starts_with('/'))
        .map(|path| path.trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty())
        // `backup /etc /etc/` records two paths naming one directory.
        // `parse_ls_children` groups by path, so the repeat would come back
        // empty and spend a filter on restic for nothing. Deduplicating before
        // the cap also means it counts distinct directories.
        .filter(|path| seen.insert(path.clone()))
        .take(PREVIEW_MAX_DIRS_PER_LEVEL)
        .collect();
    if roots.is_empty() {
        vec![PREVIEW_ROOT.to_string()]
    } else {
        roots
    }
}

/// The level-walking half of the preview, separated from restic so the
/// traversal, the entry budget and the `truncated` flag can be tested against
/// a synthetic tree.
///
/// `fetch` is handed one level's directories and returns, per directory and in
/// the same order, that directory's immediate children.
fn build_preview(
    roots: &[String],
    limit: usize,
    mut fetch: impl FnMut(&[String]) -> Result<Vec<(String, Vec<LsNode>)>>,
) -> Result<ContentsPreview> {
    let mut entries: Vec<PreviewEntry> = Vec::new();
    let mut level = roots.to_vec();
    let mut truncated = false;

    for _ in 0..preview_max_levels(limit) {
        if level.is_empty() || entries.len() >= limit {
            break;
        }
        let mut next = Vec::new();
        let mut leftovers = Vec::new();
        // First pass: a fixed slice of each directory before any directory gets
        // a second helping, so a crowded `/etc` cannot fill the whole preview
        // and leave `/home` and `/root` invisible. The cap is deliberately not
        // derived from `limit`: that keeps the ordering identical as the limit
        // grows, so "load more" extends the list instead of reshuffling it.
        for (_, children) in fetch(&level)? {
            let mut children = children.into_iter();
            for node in children.by_ref().take(PREVIEW_ENTRIES_PER_DIR) {
                if entries.len() >= limit {
                    truncated = true;
                    break;
                }
                push_entry(&mut entries, &mut next, node);
            }
            leftovers.extend(children);
        }
        // Second pass: spend whatever budget is left going deeper into the same
        // directories, in the order they were listed.
        for node in leftovers {
            if entries.len() >= limit {
                truncated = true;
                break;
            }
            push_entry(&mut entries, &mut next, node);
        }
        level = next;
    }
    // Directories still queued means there is more to show at a greater depth,
    // which a larger limit will reach.
    Ok(ContentsPreview {
        truncated: truncated || !level.is_empty(),
        entries,
    })
}

fn push_entry(entries: &mut Vec<PreviewEntry>, next: &mut Vec<String>, node: LsNode) {
    if node.kind == "dir" && next.len() < PREVIEW_MAX_DIRS_PER_LEVEL {
        next.push(node.path.clone());
    }
    entries.push(PreviewEntry {
        kind: node_kind(&node.kind),
        path: node.path,
        size: node.size,
    });
}

/// Group one `restic ls` level into `(directory, children)` pairs, ordered to
/// match `dirs`. Nodes that are not a direct child of a requested directory —
/// notably the filter directories' own nodes, which restic echoes back — are
/// dropped.
fn parse_ls_children(text: &str, dirs: &[String]) -> Result<Vec<(String, Vec<LsNode>)>> {
    let mut children: HashMap<&str, Vec<LsNode>> = HashMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).context("parsing restic ls JSON line")?;
        if value.get("message_type").and_then(|kind| kind.as_str()) != Some("node") {
            continue;
        }
        let node: LsNode = serde_json::from_value(value).context("parsing restic ls node")?;
        // A path with no parent cannot be a child of any filter directory —
        // those are all absolute — so skip it rather than failing the whole
        // preview over one unexpected line.
        let Some(parent) = parent_path(&node.path) else {
            continue;
        };
        if let Some(dir) = dirs.iter().find(|dir| *dir == &parent) {
            children.entry(dir.as_str()).or_default().push(node);
        }
    }
    Ok(dirs
        .iter()
        .map(|dir| {
            let mut nodes = children.remove(dir.as_str()).unwrap_or_default();
            // Siblings share a parent prefix, so ordering by path orders them
            // by name — the same dirs-first order list_tree produces.
            nodes.sort_by(|a, b| {
                let ad = a.kind == "dir";
                let bd = b.kind == "dir";
                bd.cmp(&ad).then_with(|| a.path.cmp(&b.path))
            });
            (dir.clone(), nodes)
        })
        .collect())
}

/// The parent of an absolute restic path, with the root spelled `/` rather
/// than the empty string so it can be compared against a filter directory.
fn parent_path(path: &str) -> Option<String> {
    let (head, _) = path.rsplit_once('/')?;
    Some(if head.is_empty() {
        PREVIEW_ROOT.to_string()
    } else {
        head.to_string()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffModifier {
    Added,
    Removed,
    Modified,
    TypeChanged,
}

impl DiffModifier {
    pub(crate) fn as_char(self) -> char {
        match self {
            DiffModifier::Added => '+',
            DiffModifier::Removed => '-',
            DiffModifier::Modified => 'M',
            DiffModifier::TypeChanged => 'T',
        }
    }
}

#[derive(Debug)]
pub(crate) struct DiffChange {
    pub(crate) modifier: DiffModifier,
    pub(crate) path: String,
}

#[derive(Debug, Default)]
pub(crate) struct DiffSummary {
    pub(crate) changed_files: u64,
    pub(crate) added_files: u64,
    pub(crate) added_bytes: u64,
    pub(crate) removed_files: u64,
    pub(crate) removed_bytes: u64,
}

#[derive(Deserialize)]
struct DiffStat {
    #[serde(default)]
    files: u64,
    #[serde(default)]
    bytes: u64,
}

pub(crate) fn diff_snapshots(
    repo: &RepoSession,
    first_id: &str,
    second_id: &str,
) -> Result<(DiffSummary, Vec<DiffChange>)> {
    let output = restic::run(&repo.profile, &["diff", "--json", first_id, second_id])?;
    let text = String::from_utf8(output).context("restic diff output was not UTF-8")?;
    let mut summary = DiffSummary::default();
    let mut changes = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).context("parsing restic diff JSON line")?;
        match value.get("message_type").and_then(|kind| kind.as_str()) {
            Some("change") => {
                let modifier = value
                    .get("modifier")
                    .and_then(|modifier| modifier.as_str())
                    .unwrap_or_default();
                let modifier = if modifier.contains('T') {
                    DiffModifier::TypeChanged
                } else if modifier.contains('+') {
                    DiffModifier::Added
                } else if modifier.contains('-') {
                    DiffModifier::Removed
                } else {
                    DiffModifier::Modified
                };
                let path = value
                    .get("path")
                    .and_then(|path| path.as_str())
                    .ok_or_else(|| anyhow!("restic diff change omitted its path"))?;
                changes.push(DiffChange {
                    modifier,
                    path: path.to_string(),
                });
            }
            Some("statistics") => {
                summary.changed_files = value
                    .get("changed_files")
                    .and_then(|count| count.as_u64())
                    .unwrap_or_default();
                let added: DiffStat = serde_json::from_value(
                    value.get("added").cloned().unwrap_or_default(),
                )
                .context("parsing restic diff added statistics")?;
                let removed: DiffStat = serde_json::from_value(
                    value.get("removed").cloned().unwrap_or_default(),
                )
                .context("parsing restic diff removed statistics")?;
                summary.added_files = added.files;
                summary.added_bytes = added.bytes;
                summary.removed_files = removed.files;
                summary.removed_bytes = removed.bytes;
            }
            _ => {}
        }
    }
    Ok((summary, changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One level of real `restic ls --json <snap> /zdir` output: a snapshot
    // header, the filter directory's own node echoed back, its children, and a
    // grandchild that a `--recursive` run would have emitted.
    const LS_FIXTURE: &str = r#"
{"message_type":"snapshot","id":"abc","short_id":"abc"}
{"message_type":"node","name":"zdir","type":"dir","path":"/zdir"}
{"message_type":"node","name":"inner.txt","type":"file","path":"/zdir/inner.txt","size":7}
{"message_type":"node","name":"link","type":"symlink","path":"/zdir/link"}
{"message_type":"node","name":"sub","type":"dir","path":"/zdir/sub"}
{"message_type":"node","name":"deep.txt","type":"file","path":"/zdir/sub/deep.txt","size":1}
"#;

    #[test]
    fn ls_children_keeps_only_direct_children_dirs_first() {
        let dirs = vec!["/zdir".to_string()];
        let levels = parse_ls_children(LS_FIXTURE, &dirs).expect("parse");
        assert_eq!(levels.len(), 1);
        let (dir, nodes) = &levels[0];
        assert_eq!(dir, "/zdir");
        let paths: Vec<&str> = nodes.iter().map(|n| n.path.as_str()).collect();
        // `/zdir` itself and the grandchild `/zdir/sub/deep.txt` are dropped;
        // the subdirectory sorts ahead of the file and the symlink.
        assert_eq!(paths, ["/zdir/sub", "/zdir/inner.txt", "/zdir/link"]);
        assert_eq!(nodes[1].size, 7);
    }

    #[test]
    fn ls_children_returns_requested_directories_in_order() {
        let text = r#"
{"message_type":"node","name":"b.txt","type":"file","path":"/b/b.txt"}
{"message_type":"node","name":"a.txt","type":"file","path":"/a/a.txt"}
"#;
        let dirs = vec!["/a".to_string(), "/b".to_string(), "/empty".to_string()];
        let levels = parse_ls_children(text, &dirs).expect("parse");
        let order: Vec<&str> = levels.iter().map(|(dir, _)| dir.as_str()).collect();
        assert_eq!(order, ["/a", "/b", "/empty"]);
        assert!(levels[2].1.is_empty());
    }

    // A synthetic snapshot shaped like the ones this preview is slow on: a few
    // top-level directories over a large, deep tree.
    fn fake_children(dir: &str) -> Vec<LsNode> {
        let node = |path: String, kind: &str| LsNode {
            kind: kind.to_string(),
            path,
            size: 0,
        };
        match dir {
            "/" => vec![
                node("/etc".into(), "dir"),
                node("/home".into(), "dir"),
                node("/root".into(), "dir"),
            ],
            "/etc" | "/home" | "/root" => (0..30)
                .map(|i| node(format!("{dir}/sub{i:02}"), "dir"))
                .collect(),
            // Every deeper directory holds 30 files and one subdirectory, so
            // the tree is effectively unbounded below this point.
            _ => (0..30)
                .map(|i| node(format!("{dir}/file{i:02}"), "file"))
                .chain(std::iter::once(node(format!("{dir}/nested"), "dir")))
                .collect(),
        }
    }

    fn fake_preview(limit: usize, calls: &mut Vec<Vec<String>>) -> ContentsPreview {
        build_preview(&[PREVIEW_ROOT.to_string()], limit, |dirs| {
            calls.push(dirs.to_vec());
            Ok(dirs
                .iter()
                .map(|dir| (dir.clone(), fake_children(dir)))
                .collect())
        })
        .expect("preview")
    }

    #[test]
    fn preview_roots_start_at_the_backed_up_paths() {
        let paths = vec!["/etc".to_string(), "/home/andrew/".to_string()];
        assert_eq!(preview_roots(&paths), ["/etc", "/home/andrew"]);
    }

    #[test]
    fn ls_children_skips_a_node_with_no_parent_path() {
        let text = r#"
{"message_type":"node","name":"odd","type":"file","path":"odd"}
{"message_type":"node","name":"a.txt","type":"file","path":"/a/a.txt"}
"#;
        let dirs = vec!["/a".to_string()];
        let levels = parse_ls_children(text, &dirs).expect("parse");
        // The parentless path belongs to no filter directory either way, so it
        // is dropped rather than failing the whole preview.
        let paths: Vec<&str> = levels[0].1.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, ["/a/a.txt"]);
    }

    #[test]
    fn preview_roots_drop_repeated_paths() {
        let paths = vec!["/etc".to_string(), "/etc/".to_string(), "/home".to_string()];
        assert_eq!(preview_roots(&paths), ["/etc", "/home"]);
    }

    #[test]
    fn preview_roots_fall_back_to_the_tree_root() {
        // Windows snapshots record `C:\…`, which is not a usable `ls` filter.
        let paths = vec![r"C:\Users\andrew\projects".to_string()];
        assert_eq!(preview_roots(&paths), ["/"]);
        assert_eq!(preview_roots(&[]), ["/"]);
    }

    #[test]
    fn preview_walks_breadth_first_from_the_root() {
        let mut calls = Vec::new();
        let preview = fake_preview(8, &mut calls);
        let paths: Vec<&str> = preview.entries.iter().map(|e| e.path.as_str()).collect();
        // The top-level directories all appear before anything nested — the
        // whole point of the preview, since they are what identifies the
        // snapshot. A depth-first walk would have spent all 8 entries inside
        // /etc and never shown /home or /root.
        assert_eq!(
            paths,
            [
                "/etc",
                "/home",
                "/root",
                "/etc/sub00",
                "/etc/sub01",
                "/etc/sub02",
                "/etc/sub03",
                "/etc/sub04",
            ]
        );
        assert!(preview.truncated);
        assert_eq!(calls, vec![vec!["/".to_string()], vec!["/etc".to_string(), "/home".to_string(), "/root".to_string()]]);
    }

    // The regression guard for the reason this was rewritten: previewing a
    // snapshot must not cost restic work proportional to the snapshot's size.
    #[test]
    fn preview_costs_one_restic_call_per_level() {
        let mut calls = Vec::new();
        let preview = fake_preview(50, &mut calls);
        assert_eq!(preview.entries.len(), 50);
        // Two restic calls cover a 50-entry preview of an unbounded tree: the
        // root, then its three children, whose 90 entries overrun the budget.
        assert_eq!(calls.len(), 2);
        // Never more directories in one call than the per-level cap, so argv
        // stays well inside the Windows command-line limit.
        assert!(calls.iter().all(|dirs| dirs.len() <= PREVIEW_MAX_DIRS_PER_LEVEL));
    }

    // A snapshot of `/etc,/home,/root` must show something from each, or the
    // preview cannot answer the question it exists to answer.
    #[test]
    fn preview_shows_every_backup_root_not_just_the_first() {
        let mut calls = Vec::new();
        let preview = fake_preview(50, &mut calls);
        for root in ["/etc/", "/home/", "/root/"] {
            assert!(
                preview.entries.iter().any(|e| e.path.starts_with(root)),
                "no entry under {root}: {:?}",
                preview.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn preview_stops_at_the_entry_budget_and_reports_more() {
        let mut calls = Vec::new();
        let preview = fake_preview(2, &mut calls);
        assert_eq!(preview.entries.len(), 2);
        assert!(preview.truncated);
    }

    #[test]
    fn raising_the_limit_reveals_more_entries() {
        // "load more" adds both entries and a level, so paginating always makes
        // progress instead of redrawing the same list.
        let (mut first, mut second) = (Vec::new(), Vec::new());
        let small = fake_preview(50, &mut first);
        let large = fake_preview(100, &mut second);
        assert!(large.entries.len() > small.entries.len());
        assert!(second.len() > first.len());
        let small_paths: Vec<&str> = small.entries.iter().map(|e| e.path.as_str()).collect();
        let large_paths: Vec<&str> = large.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(large_paths[..small_paths.len()], small_paths[..]);
    }

    // restic's tree mirrors the path that was backed up, so a snapshot of
    // `/home/andrew/projects/thing` opens with a chain of single-child
    // directories before any content appears. The walk has to spend levels on
    // that chain and still reach the files.
    #[test]
    fn preview_reaches_content_below_a_deep_single_child_chain() {
        const CHAIN: [&str; 5] = [
            "/tmp",
            "/tmp/work",
            "/tmp/work/src",
            "/tmp/work/src/app",
            "/tmp/work/src/app/nested",
        ];
        let chain_fetch = |dirs: &[String]| {
            Ok(dirs
                .iter()
                .map(|dir| {
                    let nodes = match CHAIN.iter().position(|link| link == dir) {
                        Some(i) if i + 1 < CHAIN.len() => vec![LsNode {
                            kind: "dir".into(),
                            path: CHAIN[i + 1].into(),
                            size: 0,
                        }],
                        Some(_) => vec![LsNode {
                            kind: "file".into(),
                            path: format!("{dir}/a.txt"),
                            size: 7,
                        }],
                        None if dir == PREVIEW_ROOT => vec![LsNode {
                            kind: "dir".into(),
                            path: CHAIN[0].into(),
                            size: 0,
                        }],
                        None => Vec::new(),
                    };
                    (dir.clone(), nodes)
                })
                .collect())
        };

        let mut calls = 0;
        let from_root = build_preview(&[PREVIEW_ROOT.to_string()], 50, |dirs| {
            calls += 1;
            chain_fetch(dirs)
        })
        .expect("preview");
        let paths: Vec<&str> = from_root.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths.last(), Some(&"/tmp/work/src/app/nested/a.txt"));
        assert!(!from_root.truncated);
        assert_eq!(calls, 6, "one call per link in the chain, plus the content");

        // Seeding at the snapshot's own path skips the whole chain, which is
        // the difference between six restic invocations and one.
        let mut seeded_calls = 0;
        let seeded = build_preview(&[CHAIN[4].to_string()], 50, |dirs| {
            seeded_calls += 1;
            chain_fetch(dirs)
        })
        .expect("preview");
        assert_eq!(seeded_calls, 1);
        let seeded_paths: Vec<&str> = seeded.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(seeded_paths, ["/tmp/work/src/app/nested/a.txt"]);
    }

    #[test]
    fn preview_is_not_truncated_when_the_snapshot_fits() {
        let preview = build_preview(&[PREVIEW_ROOT.to_string()], 50, |dirs| {
            Ok(dirs
                .iter()
                .map(|dir| {
                    let nodes = if dir == PREVIEW_ROOT {
                        vec![LsNode {
                            kind: "file".into(),
                            path: "/only.txt".into(),
                            size: 3,
                        }]
                    } else {
                        Vec::new()
                    };
                    (dir.clone(), nodes)
                })
                .collect())
        })
        .expect("preview");
        assert_eq!(preview.entries.len(), 1);
        assert!(!preview.truncated);
    }

    // A tree holding `/src/a.txt` and nothing at the seeded path, which is what
    // a snapshot taken from a relative path looks like: restic absolutises
    // `paths` to `/home/andrew/deep/src` but stores the tree as `/src`.
    fn relative_backup_fetch(dirs: &[String]) -> Result<Vec<(String, Vec<LsNode>)>> {
        Ok(dirs
            .iter()
            .map(|dir| {
                let node = |path: &str, kind: &str| LsNode {
                    kind: kind.to_string(),
                    path: path.to_string(),
                    size: 0,
                };
                let nodes = match dir.as_str() {
                    "/" => vec![node("/src", "dir")],
                    "/src" => vec![node("/src/a.txt", "file")],
                    _ => Vec::new(),
                };
                (dir.clone(), nodes)
            })
            .collect())
    }

    #[test]
    fn preview_falls_back_to_the_tree_root_when_the_seeded_paths_list_nothing() {
        let mut asked = Vec::new();
        let preview = preview_from_paths(&["/home/andrew/deep/src".to_string()], 50, |dirs| {
            asked.push(dirs.to_vec());
            relative_backup_fetch(dirs)
        })
        .expect("preview");
        let paths: Vec<&str> = preview.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/src", "/src/a.txt"]);
        assert_eq!(asked[0], ["/home/andrew/deep/src"], "seeded path tried first");
        assert_eq!(asked[1], ["/"], "then the walk restarts at the tree root");
    }

    #[test]
    fn preview_keeps_the_seeded_roots_when_they_list_something() {
        let mut asked = Vec::new();
        let preview = preview_from_paths(&["/src".to_string()], 50, |dirs| {
            asked.push(dirs.to_vec());
            relative_backup_fetch(dirs)
        })
        .expect("preview");
        let paths: Vec<&str> = preview.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/src/a.txt"]);
        // No restart from `/`: the seed worked, so the extra call is not spent.
        assert!(!asked.iter().any(|dirs| dirs == &[PREVIEW_ROOT.to_string()]));
    }

    // ──── Tree cache ────────────────────────────────────────────────────

    fn tree_id(byte: &str) -> String {
        byte.repeat(32)
    }

    // A directory holding one subdirectory and one file.
    fn tree_json(subtree: &str) -> Vec<u8> {
        format!(
            r#"{{"nodes":[
                {{"name":"sub","type":"dir","subtree":"{subtree}"}},
                {{"name":"a.txt","type":"file","size":3}}
            ]}}"#
        )
        .into_bytes()
    }

    fn test_session(trees: TreeCache) -> RepoSession {
        let profile = Profile::Local {
            password: "pw".into(),
            local_path: "tmp/never-touched".into(),
        };
        open_indexed(&profile, trees).expect("session")
    }

    #[test]
    fn tree_cache_serves_a_second_read_without_a_restic_call() {
        let root = tree_id("aa");
        let repo = test_session(TreeCache::default());
        repo.register_selector(root.clone(), "snap:/".into()).unwrap();

        let mut calls = 0;
        let mut load = |repo: &RepoSession| {
            load_tree_with(repo, &root, |_| {
                calls += 1;
                Ok(tree_json(&tree_id("bb")))
            })
            .expect("tree")
        };
        let first = load(&repo);
        let second = load(&repo);
        assert_eq!(calls, 1, "the second read must come out of the cache");
        assert!(Arc::ptr_eq(&first, &second), "and must be the same object");
    }

    // The payoff: an incremental backup reuses the tree of every unchanged
    // directory, so the second snapshot browsed reads them out of the cache.
    #[test]
    fn tree_cache_is_shared_across_sessions() {
        let root = tree_id("aa");
        let trees = TreeCache::default();

        let first = test_session(trees.clone());
        first.register_selector(root.clone(), "snap1:/".into()).unwrap();
        load_tree_with(&first, &root, |_| Ok(tree_json(&tree_id("bb")))).expect("first");

        let second = test_session(trees);
        second.register_selector(root.clone(), "snap2:/".into()).unwrap();
        load_tree_with(&second, &root, |_| panic!("must not refetch a cached tree"))
            .expect("second");
    }

    // A cached tree carries no selectors with it, and a selector is only valid
    // for the snapshot it was built under. Listing must therefore re-register
    // every child under the current snapshot even when nothing was fetched —
    // otherwise descending into the directory fails with "no restic snapshot
    // path registered".
    #[test]
    fn listing_a_cached_tree_still_registers_children_for_this_snapshot() {
        let root = tree_id("aa");
        let child = tree_id("bb");
        let trees = TreeCache::default();

        let first = test_session(trees.clone());
        first.register_selector(root.clone(), "snap1:/".into()).unwrap();
        load_tree_with(&first, &root, |_| Ok(tree_json(&child))).expect("prime the cache");
        let rows = list_tree(&first, &root).expect("rows");
        assert_eq!(rows[0].subtree.as_deref(), Some(child.as_str()));
        assert_eq!(first.selector_for(&child).unwrap(), "snap1:/sub");

        let second = test_session(trees);
        second.register_selector(root.clone(), "snap2:/".into()).unwrap();
        list_tree(&second, &root).expect("rows from the cached tree");
        assert_eq!(second.selector_for(&child).unwrap(), "snap2:/sub");
    }

    #[test]
    fn tree_cache_stops_growing_at_its_cap() {
        let trees = TreeCache::default();
        let empty = || Arc::new(TreeDocument { nodes: Vec::new() });
        for i in 0..TREE_CACHE_MAX_TREES + 10 {
            trees.insert(&format!("{i:064x}"), empty()).unwrap();
        }
        assert_eq!(trees.len(), TREE_CACHE_MAX_TREES);
        // What is held is what was read first, which is what a browse walks
        // back through: the trees nearest the roots already opened.
        assert!(trees.get(&format!("{:064x}", 0)).unwrap().is_some());
    }

    #[test]
    #[ignore]
    fn live_restic_metadata_round_trip() {
        use std::fs;
        use std::path::PathBuf;

        let root = PathBuf::from("tmp").join(format!("restic-read-it-{}", std::process::id()));
        let repository = root.join("repo");
        let source = root.join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/a.txt"), b"first\n").unwrap();

        let profile = Profile::Local {
            password: "pw".into(),
            local_path: repository.to_string_lossy().into_owned(),
        };
        restic::run(&profile, &["init"]).expect("init");
        restic::run(&profile, &["backup", source.to_str().unwrap()]).expect("first backup");
        fs::write(source.join("nested/a.txt"), b"second\n").unwrap();
        fs::write(source.join("nested/b.txt"), b"added\n").unwrap();
        restic::run(&profile, &["backup", source.to_str().unwrap()]).expect("second backup");

        verify_profile(&profile).expect("verify");
        let snapshots = load_snapshots(&profile).expect("snapshots");
        assert_eq!(snapshots.len(), 2);

        let trees = TreeCache::default();
        let session = open_indexed(&profile, trees.clone()).expect("session");
        let root_tree = snapshot_root_tree(&session, &snapshots[0].id).expect("root tree");
        let root_rows = list_tree(&session, &root_tree).expect("root rows");
        assert!(!root_rows.is_empty());

        // A second session over the same cache reads the same tree without
        // fetching it again, and still resolves its children.
        let reopened = open_indexed(&profile, trees.clone()).expect("second session");
        let cached_at = trees.len();
        let same_root = snapshot_root_tree(&reopened, &snapshots[0].id).expect("root tree again");
        assert_eq!(same_root, root_tree);
        let cached_rows = list_tree(&reopened, &same_root).expect("rows from cache");
        let names = |rows: &[ContentRow]| {
            rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(names(&cached_rows), names(&root_rows));
        assert_eq!(trees.len(), cached_at, "listing again must not add trees");

        let preview =
            preview_snapshot_contents(&session, &snapshots[0].id, &snapshots[0].paths, 50)
                .expect("preview");
        assert!(preview.entries.iter().any(|entry| entry.path.ends_with("/a.txt")));
        assert!(preview.entries.iter().any(|entry| entry.path.ends_with("/b.txt")));

        let (summary, changes) =
            diff_snapshots(&session, &snapshots[1].id, &snapshots[0].id).expect("diff");
        assert!(summary.changed_files > 0 || !changes.is_empty());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    #[ignore]
    fn live_garage_s3_profile_reads_seeded_repository() {
        let endpoint = std::env::var("RESTERM_GARAGE_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:3900".into());
        let profile = Profile::S3 {
            password: "garage-repository-password".into(),
            s3_endpoint: endpoint,
            s3_bucket: "resterm-it".into(),
            s3_region: "garage".into(),
            s3_root: "repository".into(),
            s3_access_key: "GK22222222222222222222222222222222".into(),
            s3_secret_key:
                "3333333333333333333333333333333333333333333333333333333333333333".into(),
        };

        verify_profile(&profile).expect("verify Garage profile");
        let snapshots = load_snapshots(&profile).expect("list Garage snapshots");
        let snapshot = snapshots.first().expect("seeded Garage snapshot");
        let previous = snapshots.get(1).expect("second seeded Garage snapshot");
        assert!(snapshot.tags.iter().any(|tag| tag == "garage-e2e-second"));

        let session = open_indexed(&profile, TreeCache::default()).expect("open Garage session");
        let (summary, changes) =
            diff_snapshots(&session, &previous.id, &snapshot.id).expect("diff Garage snapshots");
        assert!(summary.changed_files > 0);
        assert!(changes.iter().any(|change| change.path.ends_with("/hello.txt")));
        assert!(changes.iter().any(|change| change.path.ends_with("/second.txt")));

        let preview = preview_snapshot_contents(&session, &snapshot.id, &snapshot.paths, 100)
            .expect("preview Garage tree");
        let hello = preview
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("/hello.txt"))
            .expect("hello.txt in Garage snapshot");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let dump_profile = profile.clone();
        let dump_snapshot = snapshot.id.clone();
        let dump_path = hello.path.clone();
        let dump = std::thread::spawn(move || {
            restic::stream_dump(&dump_profile, &dump_snapshot, &dump_path, &tx)
        });
        let mut bytes = Vec::new();
        while let Some(chunk) = rx.blocking_recv() {
            bytes.extend_from_slice(&chunk.expect("Garage dump chunk"));
        }
        dump.join()
            .expect("Garage dump thread")
            .expect("stream Garage hello.txt");
        assert_eq!(bytes, b"hello from Garage S3 integration, revision 2\n");
    }
}

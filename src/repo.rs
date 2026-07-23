use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

#[derive(Clone)]
pub(crate) struct RepoSession {
    profile: Profile,
    tree_selectors: Arc<Mutex<HashMap<String, String>>>,
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

#[derive(Deserialize)]
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

pub(crate) fn open_indexed(profile: &Profile) -> Result<RepoSession> {
    Ok(RepoSession {
        profile: profile.clone(),
        tree_selectors: Arc::new(Mutex::new(HashMap::new())),
    })
}

fn load_tree(repo: &RepoSession, tree_id: &str) -> Result<TreeDocument> {
    let selector = repo
        .tree_selectors
        .lock()
        .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
        .get(tree_id)
        .cloned()
        .ok_or_else(|| anyhow!("no restic snapshot path registered for tree `{tree_id}`"))?;
    let output = restic::run(&repo.profile, &["cat", "tree", &selector, "--json"])?;
    serde_json::from_slice(&output)
        .with_context(|| format!("parsing restic tree JSON for `{selector}`"))
}

pub(crate) fn snapshot_root_tree(repo: &RepoSession, snapshot_id: &str) -> Result<String> {
    let (snapshot, _) = restic::snapshot_details_json(&repo.profile, snapshot_id)?;
    let tree = snapshot
        .tree
        .ok_or_else(|| anyhow!("snapshot `{snapshot_id}` has no root tree"))?;
    let tree_id = parse_tree_id(&tree)?;
    repo.tree_selectors
        .lock()
        .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
        .insert(tree_id.clone(), format!("{snapshot_id}:/"));
    Ok(tree_id)
}

pub(crate) fn list_tree(repo: &RepoSession, tree_id: &str) -> Result<Vec<ContentRow>> {
    let parent_selector = repo
        .tree_selectors
        .lock()
        .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
        .get(tree_id)
        .cloned()
        .ok_or_else(|| anyhow!("no restic snapshot path registered for tree `{tree_id}`"))?;
    let mut rows = load_tree(repo, tree_id)?
        .nodes
        .into_iter()
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
                repo.tree_selectors
                    .lock()
                    .map_err(|_| anyhow!("restic tree selector cache was poisoned"))?
                    .insert(subtree.clone(), format!("{snapshot}:{child_path}"));
            }
            Ok(ContentRow {
                name: node.name,
                kind: node_kind(&node.kind),
                size: node.size,
                mtime: display_time(node.mtime),
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
        .into_iter()
        .find(|node| node.name == file_name)
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

pub(crate) fn preview_snapshot_contents(
    repo: &RepoSession,
    snapshot_id: &str,
    limit: usize,
) -> Result<ContentsPreview> {
    let output = restic::ls_json(&repo.profile, snapshot_id)?;
    let text = String::from_utf8(output).context("restic ls output was not UTF-8")?;
    parse_ls_preview(&text, limit)
}

fn parse_ls_preview(text: &str, limit: usize) -> Result<ContentsPreview> {
    let mut children: HashMap<String, Vec<LsNode>> = HashMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).context("parsing restic ls JSON line")?;
        if value.get("message_type").and_then(|kind| kind.as_str()) != Some("node") {
            continue;
        }
        let node: LsNode =
            serde_json::from_value(value).context("parsing restic ls node")?;
        let parent = node
            .path
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .ok_or_else(|| anyhow!("restic ls node path `{}` has no parent", node.path))?;
        children.entry(parent).or_default().push(node);
    }
    for nodes in children.values_mut() {
        // Paths within one directory share their parent prefix, so comparing
        // paths orders siblings by name — same order list_tree produces.
        nodes.sort_by(|a, b| {
            let ad = a.kind == "dir";
            let bd = b.kind == "dir";
            bd.cmp(&ad).then_with(|| a.path.cmp(&b.path))
        });
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    walk_preview(&children, "", &mut entries, limit, &mut truncated);
    Ok(ContentsPreview { entries, truncated })
}

fn walk_preview(
    children: &HashMap<String, Vec<LsNode>>,
    dir: &str,
    out: &mut Vec<PreviewEntry>,
    limit: usize,
    truncated: &mut bool,
) {
    let Some(rows) = children.get(dir) else {
        return;
    };
    for node in rows {
        if out.len() >= limit {
            *truncated = true;
            break;
        }
        out.push(PreviewEntry {
            path: node.path.clone(),
            kind: node_kind(&node.kind),
            size: node.size,
        });
        if node.kind == "dir" {
            walk_preview(children, &node.path, out, limit, truncated);
        }
    }
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

    const LS_FIXTURE: &str = r#"
{"message_type":"snapshot","id":"abc","short_id":"abc"}
{"message_type":"node","name":"a.txt","type":"file","path":"/a.txt","size":3}
{"message_type":"node","name":"zdir","type":"dir","path":"/zdir"}
{"message_type":"node","name":"inner.txt","type":"file","path":"/zdir/inner.txt","size":7}
{"message_type":"node","name":"link","type":"symlink","path":"/zdir/link"}
"#;

    #[test]
    fn ls_preview_orders_dirs_first_and_recurses() {
        let preview = parse_ls_preview(LS_FIXTURE, 10).expect("parse");
        let paths: Vec<&str> = preview.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/zdir", "/zdir/inner.txt", "/zdir/link", "/a.txt"]);
        assert!(!preview.truncated);
        assert!(matches!(preview.entries[0].kind, ContentKind::Dir));
        assert_eq!(preview.entries[1].size, 7);
        assert!(matches!(preview.entries[2].kind, ContentKind::Symlink));
    }

    #[test]
    fn ls_preview_truncates_at_limit() {
        let preview = parse_ls_preview(LS_FIXTURE, 2).expect("parse");
        let paths: Vec<&str> = preview.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/zdir", "/zdir/inner.txt"]);
        assert!(preview.truncated);
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

        let session = open_indexed(&profile).expect("session");
        let root_tree = snapshot_root_tree(&session, &snapshots[0].id).expect("root tree");
        let root_rows = list_tree(&session, &root_tree).expect("root rows");
        assert!(!root_rows.is_empty());

        let preview =
            preview_snapshot_contents(&session, &snapshots[0].id, 50).expect("preview");
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
        let endpoint = std::env::var("WRUSTIC_GARAGE_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:3900".into());
        let profile = Profile::S3 {
            password: "garage-repository-password".into(),
            s3_endpoint: endpoint,
            s3_bucket: "wrustic-it".into(),
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

        let session = open_indexed(&profile).expect("open Garage session");
        let (summary, changes) =
            diff_snapshots(&session, &previous.id, &snapshot.id).expect("diff Garage snapshots");
        assert!(summary.changed_files > 0);
        assert!(changes.iter().any(|change| change.path.ends_with("/hello.txt")));
        assert!(changes.iter().any(|change| change.path.ends_with("/second.txt")));

        let preview =
            preview_snapshot_contents(&session, &snapshot.id, 100).expect("preview Garage tree");
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

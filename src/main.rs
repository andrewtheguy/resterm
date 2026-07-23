mod app;
mod cli;
mod config;
mod crypto;
#[cfg(feature = "keychain")]
mod keychain;
mod local_server;
mod passphrase;
mod repo;
mod restic;
mod share;
mod ui;

use std::path::PathBuf;
use anyhow::Result;
use ratatui::{
    DefaultTerminal,
    crossterm::{
        self,
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    },
};

use ratatui::widgets::TableState;

use crate::app::{App, BrowseFrame, Screen};
use crate::cli::{USAGE, parse_cli};
use crate::repo::{
    ContentRow, diff_snapshots, get_file_details, list_tree, load_snapshots, open_indexed,
    preview_snapshot_contents, snapshot_root_tree, verify_profile,
};
use crate::ui::render;

fn main() -> Result<()> {
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("{e:#}");
            eprintln!("\n{USAGE}");
            std::process::exit(2);
        }
    };
    if cli.show_version {
        println!("wrustic {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if cli.show_help {
        println!("{USAGE}");
        return Ok(());
    }
    if let Err(error) = restic::detect() {
        eprintln!("{}", error.user_message());
        std::process::exit(1);
    }

    #[cfg(feature = "keychain")]
    let no_keychain = cli.no_keychain || !keychain::init_store();
    #[cfg(not(feature = "keychain"))]
    let no_keychain = cli.no_keychain;

    let mut terminal = ratatui::init();
    // Enable mouse reporting after entering raw mode. With capture on,
    // terminals route clicks/scroll to us instead of doing native text
    // selection; users can hold Shift to bypass and select text.
    let mouse_enabled = !cli.no_mouse
        && crossterm::execute!(std::io::stdout(), EnableMouseCapture).is_ok();
    let result = run(&mut terminal, cli.config_dir, cli.port, no_keychain);
    if mouse_enabled {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    result
}

fn run(
    terminal: &mut DefaultTerminal,
    config_dir: Option<PathBuf>,
    server_port: u16,
    no_keychain: bool,
) -> Result<()> {
    let mut app = App::boot(config_dir, server_port, no_keychain)?;

    while !app.quit {
        terminal.draw(|f| render(f, &mut app))?;

        if matches!(app.screen, Screen::Loading) {
            let idx = app.loading_index;
            let Some((_, profile)) = app.config.profile_at(idx) else {
                app.screen = Screen::Error("Selected profile no longer exists.".into());
                continue;
            };
            match load_snapshots(profile) {
                Ok(snaps) => {
                    app.snapshots = snaps;
                    let len = app.visible_snapshot_indices().len();
                    if len > 0 {
                        let idx = app
                            .post_delete_select
                            .map(|i| i.min(len - 1))
                            .unwrap_or(0);
                        app.list_state.select(Some(idx));
                    } else {
                        app.list_state.select(None);
                    }
                    app.post_delete_select = None;
                    app.screen = Screen::Snapshots;
                }
                Err(e) => {
                    app.screen = Screen::Error(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::PassphraseDerivingKey) {
            app.derive_passphrase_key();
            continue;
        }

        if matches!(app.screen, Screen::Verifying) {
            let profile = app.build_profile();
            match verify_profile(&profile) {
                Ok(()) => app.commit_profile(),
                Err(e) => app.screen = Screen::VerifyFailed(format!("{e:#}")),
            }
            continue;
        }

        if matches!(app.screen, Screen::OpeningSnapshot) {
            let idx = app.loading_index;
            let Some((_, profile)) = app.config.profile_at(idx) else {
                app.screen = Screen::Error("Selected profile no longer exists.".into());
                continue;
            };
            let snap_id = app.browse_snapshot_id.clone();
            let refresh_path = app.pending_refresh_path.take();
            match open_and_walk(profile, &snap_id, refresh_path.as_deref()) {
                Ok((repo, stack)) => {
                    app.repo_session = Some(repo);
                    app.browse_stack = stack;
                    app.screen = Screen::SnapshotContents;
                }
                Err(e) => {
                    app.repo_session = None;
                    app.browse_stack.clear();
                    app.screen = Screen::Error(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::SnapshotDeleteLoading) {
            let Some(snap_id) = app.delete_target.clone() else {
                app.screen = Screen::SnapshotDeleteError(
                    "No snapshot selected for deletion.".into(),
                );
                continue;
            };
            let idx = app.loading_index;
            let Some((_, profile)) = app.config.profile_at(idx) else {
                app.screen = Screen::SnapshotDeleteError(
                    "Selected profile no longer exists.".into(),
                );
                continue;
            };
            let limit = app.delete_preview_limit;
            let need_details = app.delete_info.is_none();
            let cached_preview = app.preview_cache.get(&(snap_id.clone(), limit)).cloned();
            let result = (|| -> anyhow::Result<_> {
                let repo = open_indexed(profile)?;
                let restic_details = if need_details {
                    Some(restic::snapshot_details_json(profile, &snap_id)?)
                } else {
                    None
                };
                let info = restic_details.as_ref().map(|(parsed, _)| {
                    crate::repo::DeleteSnapshotInfo {
                        hostname: parsed.hostname.clone().unwrap_or_default(),
                        paths: parsed.paths.clone(),
                        tags: parsed.tags.clone(),
                    }
                });
                let preview = match cached_preview {
                    Some(preview) => preview,
                    None => preview_snapshot_contents(&repo, &snap_id, limit)?,
                };
                Ok((info, restic_details, preview))
            })();
            match result {
                Ok((info, restic_details, preview)) => {
                    let has_entries = !preview.entries.is_empty();
                    if let Some(info) = info {
                        app.delete_info = Some(info);
                    }
                    if let Some((parsed, raw)) = restic_details {
                        app.delete_details_parsed = Some(parsed);
                        app.delete_details_raw = Some(raw);
                    }
                    app.preview_cache
                        .insert((snap_id.clone(), limit), preview.clone());
                    app.delete_root_listing = Some(preview);
                    app.delete_preview_state = TableState::default();
                    if has_entries {
                        app.delete_preview_state.select(Some(0));
                    }
                    app.screen = Screen::SnapshotDeleteConfirm;
                }
                Err(e) => {
                    app.screen = Screen::SnapshotDeleteError(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::SnapshotDeleting) {
            let Some(snapshot_id) = app.delete_target.take() else {
                app.screen = Screen::SnapshotDeleteError(
                    "No snapshot selected for deletion.".into(),
                );
                continue;
            };
            let idx = app.loading_index;
            let Some((_, profile)) = app.config.profile_at(idx) else {
                app.screen = Screen::SnapshotDeleteError(
                    "Selected profile no longer exists.".into(),
                );
                continue;
            };
            match restic::forget(profile, &snapshot_id) {
                Ok(()) => {
                    app.post_delete_select = app.list_state.selected();
                    app.delete_details_parsed = None;
                    app.delete_details_raw = None;
                    app.preview_cache.retain(|(id, _), _| id != &snapshot_id);
                    app.snapshots.clear();
                    app.screen = Screen::Loading;
                }
                Err(e) => {
                    app.delete_details_parsed = None;
                    app.delete_details_raw = None;
                    app.screen = Screen::SnapshotDeleteError(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::SnapshotCompareLoading) {
            let first = app.compare_first_id.clone();
            let second = app.compare_second_id.clone();
            let idx = app.loading_index;
            let Some((_, profile)) = app.config.profile_at(idx) else {
                app.clear_compare_scratch();
                app.screen = Screen::Error("Selected profile no longer exists.".into());
                continue;
            };
            let (Some(first), Some(second)) = (first, second) else {
                app.clear_compare_scratch();
                app.screen =
                    Screen::Error("Compare flow lost a snapshot id mid-flight.".into());
                continue;
            };
            let diff_result = (|| {
                let repo = open_indexed(profile)?;
                diff_snapshots(&repo, &first, &second)
            })();
            match diff_result {
                Ok((sum, changes)) => {
                    let has_rows = !changes.is_empty();
                    app.compare_results = Some((sum, changes));
                    app.compare_results_state = TableState::default();
                    if has_rows {
                        app.compare_results_state.select(Some(0));
                    }
                    app.screen = Screen::SnapshotCompareResults;
                }
                Err(e) => {
                    app.clear_compare_scratch();
                    app.screen = Screen::Error(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::LoadingDir) {
            let Some((tree_id, name)) = app.pending_descend.take() else {
                app.screen = Screen::SnapshotContents;
                continue;
            };
            let Some(repo) = app.repo_session.as_ref() else {
                app.screen = Screen::Error("Repository session was dropped.".into());
                continue;
            };
            match list_tree(repo, &tree_id) {
                Ok(items) => {
                    let (items, table_state) = with_parent(items);
                    app.browse_stack.push(BrowseFrame {
                        name,
                        tree_id,
                        items,
                        table_state,
                    });
                    app.screen = Screen::SnapshotContents;
                }
                Err(e) => {
                    app.repo_session = None;
                    app.browse_stack.clear();
                    app.screen = Screen::Error(format!("{e:#}"));
                }
            }
            continue;
        }

        if matches!(app.screen, Screen::LoadingFileDetails) {
            let Some((tree_id, name, full_path)) = app.pending_file_lookup.take() else {
                app.screen = Screen::SnapshotContents;
                continue;
            };
            let Some(repo) = app.repo_session.as_ref() else {
                app.screen = Screen::Error("Repository session was dropped.".into());
                continue;
            };
            match get_file_details(repo, &tree_id, &name, full_path) {
                Ok(details) => {
                    app.file_details = Some(details);
                    app.screen = Screen::FileDetails;
                }
                Err(e) => {
                    app.screen = Screen::Error(format!("{e:#}"));
                }
            }
            continue;
        }

        // Idle: block on events and only break out (to redraw) for ones
        // that can change what's on screen. Ignoring focus/mouse/key-release
        // events keeps the terminal quiet when the app has nothing to do.
        loop {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                    break;
                }
                Event::Mouse(m) => {
                    app.handle_mouse(m);
                    break;
                }
                Event::Resize(_, _) => break,
                _ => {}
            }
        }
    }
    Ok(())
}

fn open_and_walk(
    profile: &crate::config::Profile,
    snapshot_id: &str,
    refresh_path: Option<&[String]>,
) -> Result<(
    crate::repo::RepoSession,
    Vec<BrowseFrame>,
)> {
    let repo = open_indexed(profile)?;
    let root_tree = snapshot_root_tree(&repo, snapshot_id)?;
    let root_items = list_tree(&repo, &root_tree)?;
    let (root_items, root_table_state) = with_parent(root_items);
    let mut stack = vec![BrowseFrame {
        name: String::new(),
        tree_id: root_tree,
        items: root_items,
        table_state: root_table_state,
    }];

    if let Some(path) = refresh_path {
        for name in path {
            let top = stack.last().expect("root frame present");
            let next = top
                .items
                .iter()
                .find(|row| row.name == *name && row.subtree.is_some())
                .map(|row| (row.subtree.clone().unwrap(), row.name.clone()));
            match next {
                Some((tree_id, name)) => {
                    let items = list_tree(&repo, &tree_id)?;
                    let (items, table_state) = with_parent(items);
                    stack.push(BrowseFrame {
                        name,
                        tree_id,
                        items,
                        table_state,
                    });
                }
                None => break,
            }
        }
    }

    Ok((repo, stack))
}

// Prepend a synthetic `..` row and pick a default selection: the first real
// entry if any, otherwise the `..` row itself.
fn with_parent(items: Vec<ContentRow>) -> (Vec<ContentRow>, TableState) {
    let mut out = Vec::with_capacity(items.len() + 1);
    out.push(ContentRow::parent());
    out.extend(items);
    let mut table_state = TableState::default();
    let initial = if out.len() > 1 { 1 } else { 0 };
    table_state.select(Some(initial));
    (out, table_state)
}

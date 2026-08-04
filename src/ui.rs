use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, HighlightSpacing, List, ListItem, Paragraph, Row, Table, TableState, Wrap},
};
use tui_input::Input;

use crate::app::{App, BACKEND_ORDER, Screen, filter_dim_entries};
use crate::repo::ContentKind;

pub(crate) fn render(frame: &mut Frame, app: &mut App) {
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    render_top_bar(frame, app, top);
    render_body(frame, app, body);
    render_bottom_bar(frame, app, bottom);
}

fn render_top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let text = match &app.active_profile_name {
        Some(name) => format!("resterm — profile: {name}"),
        None => "resterm".to_string(),
    };
    let para = Paragraph::new(text)
        .style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, area);
}

fn render_bottom_bar(frame: &mut Frame, app: &App, area: Rect) {
    let text = bottom_bar_text(app);
    let content_width = area.width.saturating_sub(2) as usize;
    let segments: Vec<&str> = text.split("  ").collect();
    let footer_line = build_footer_line(&segments, content_width);
    let para = Paragraph::new(footer_line)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, area);
}

fn build_footer_line(segments: &[&str], width: usize) -> Line<'static> {
    let style = Style::default().fg(Color::White);

    if segments.is_empty() {
        return Line::from(Span::styled("", style));
    }

    let sep = " | ";
    let sep_len = sep.len();

    let mut total_width = 0usize;
    let mut fit_count = 0usize;
    for (i, seg) in segments.iter().enumerate() {
        let needed = if i == 0 { seg.len() } else { sep_len + seg.len() };
        if total_width + needed > width {
            break;
        }
        total_width += needed;
        fit_count += 1;
    }

    if fit_count == 0 {
        let first = segments[0];
        let truncated: String = first.chars().take(width).collect();
        return Line::from(Span::styled(truncated, style));
    }

    let joined = segments[..fit_count].join(sep);
    Line::from(Span::styled(joined, style))
}

fn bottom_bar_text(app: &App) -> &'static str {
    match &app.screen {
        Screen::Home => "Up/Dn move  PgUp/PgDn page  Enter open  n new  e edit  d delete  q quit",
        Screen::Snapshots => {
            "Up/Dn move  PgUp/PgDn page  g/G top/bottom  Enter browse  c compare  f filter  d delete  r refresh  q/Esc back"
        }
        Screen::SnapshotFilterDim => "Up/Dn move  PgUp/PgDn page  Enter pick  Esc back",
        Screen::SnapshotFilterValue => {
            "Up/Dn move  PgUp/PgDn page  g/G top/bottom  Enter pick  Esc back"
        }
        Screen::SnapshotDeleteConfirm => {
            "y confirm delete  Up/Dn scroll  PgUp/PgDn page  r raw JSON  n/Esc cancel"
        }
        Screen::SnapshotDeleteError(msg) => {
            if crate::restic::is_lock_error(msg) {
                "u run `restic unlock` and retry  any other key to continue"
            } else {
                "any key to continue"
            }
        }
        Screen::SnapshotContents => {
            "Up/Dn move  PgUp/PgDn page  g/G top/bottom  Enter open  Backspace up  r reload  q/Esc back"
        }
        Screen::FileDetails => {
            "Up/Dn scroll  PgUp/PgDn page  g top  s share  Esc/Backspace/q back"
        }
        Screen::ShareUrl => "Esc/Backspace/q back (stops the server)",
        Screen::PassphraseInstancePrompt => "type  Enter submit  Esc quit",
        Screen::PassphraseSetup => if app.keychain_enabled() {
            "Tab/Shift+Tab field  Space toggle  Enter submit  Esc back"
        } else {
            "Tab/Shift+Tab field  Enter submit  Esc back"
        },
        Screen::PassphraseUnlock => if app.keychain_enabled() {
            "Tab/Shift+Tab field  Space toggle  Enter submit  Esc quit"
        } else {
            "type  Enter submit  Esc quit"
        },
        Screen::PassphraseDerivingKey => "working…",
        Screen::AuthMethodChoice => "Up/Dn move  Enter pick  Esc back",
        Screen::SnapshotCompareSecond => {
            "Up/Dn move  PgUp/PgDn page  g/G top/bottom  Enter pick SECOND  a toggle related/all  Esc cancel"
        }
        Screen::SnapshotCompareResults => "Up/Dn move  PgUp/PgDn page  g/G top/bottom  q/Esc back",
        Screen::OpeningSnapshot
        | Screen::LoadingDir
        | Screen::LoadingFileDetails
        | Screen::Loading
        | Screen::Verifying
        | Screen::SnapshotDeleting
        | Screen::SnapshotDeleteLoading
        | Screen::ResticUnlocking
        | Screen::SnapshotCompareLoading => "working…",
        Screen::CreateProfileName => "type  Enter submit  Esc cancel",
        Screen::BackendChoice => "Up/Dn move  PgUp/PgDn page  Enter pick  Esc back",
        Screen::LocalPath => "type  Enter submit  Esc back",
        Screen::RestConfig | Screen::S3Location | Screen::S3Credentials => {
            "Tab/Shift+Tab field  Enter continue  Esc back"
        }
        Screen::Password => "type  Enter save  Esc back",
        Screen::ConfirmDelete => "y confirm  n/Esc cancel",
        Screen::VerifyFailed(_) => "r retry  s save anyway  Esc discard",
        Screen::Error(_) => "any key to continue",
    }
}


fn render_body(frame: &mut Frame, app: &mut App, area: Rect) {
    app.list_header_rows = 0;
    match &app.screen {
        Screen::Home => render_home(frame, app, area),
        Screen::CreateProfileName => render_input(
            frame,
            area,
            "Profile name",
            &app.new_profile_name,
            false,
            "Give this profile a short name, e.g. 'laptop-local'",
        ),
        Screen::BackendChoice => render_backend_choice(frame, app, area),
        Screen::LocalPath => render_input(
            frame,
            area,
            &profile_title("Local repository path", app),
            &app.local_path,
            false,
            if cfg!(windows) {
                "Filesystem path, e.g. C:\\restic\\my-repo"
            } else {
                "Filesystem path, e.g. /tmp/resterm-test-repo"
            },
        ),
        Screen::RestConfig => render_rest_config(frame, app, area),
        Screen::S3Location => render_s3_location(frame, app, area),
        Screen::S3Credentials => render_s3_credentials(frame, app, area),
        Screen::Password => {
            render_input(
                frame,
                area,
                &profile_title("Repository password", app),
                &app.password,
                true,
                "Restic repository password",
            );
        }
        Screen::ConfirmDelete => {
            let name = app
                .pending_delete
                .and_then(|i| app.config.name_at(i))
                .map(String::as_str)
                .unwrap_or("(unknown)");
            let body = format!("Delete profile '{name}'?");
            let para = Paragraph::new(body)
                .style(Style::new().fg(Color::Yellow))
                .block(Block::bordered().title("Confirm delete"));
            frame.render_widget(para, area);
        }
        Screen::Loading => {
            let para = Paragraph::new("Opening repository and reading snapshots…")
                .block(Block::bordered().title("Loading"));
            frame.render_widget(para, area);
        }
        Screen::Verifying => {
            let para = Paragraph::new("Verifying profile — opening repository with the entered credentials…")
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title("Verifying"));
            frame.render_widget(para, area);
        }
        Screen::VerifyFailed(msg) => {
            let body = format!(
                "Could not open the repository with this profile:\n\n{msg}",
            );
            let para = Paragraph::new(body)
                .style(Style::new().fg(Color::Yellow))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title("Verification failed"));
            frame.render_widget(para, area);
        }
        Screen::Snapshots => render_snapshots(frame, app, area),
        Screen::SnapshotFilterDim => render_filter_dim(frame, app, area),
        Screen::SnapshotFilterValue => render_filter_value(frame, app, area),
        Screen::SnapshotDeleteConfirm => render_snapshot_delete_confirm(frame, &mut *app, area),
        Screen::SnapshotDeleting => {
            let para = Paragraph::new("Running `restic forget`…")
                .block(Block::bordered().title("Deleting snapshot"));
            frame.render_widget(para, area);
        }
        Screen::SnapshotDeleteLoading => {
            let para = Paragraph::new("Loading snapshot details…")
                .block(Block::bordered().title("Loading"));
            frame.render_widget(para, area);
        }
        Screen::SnapshotDeleteError(msg) => {
            let body = if crate::restic::is_lock_error(msg) {
                format!(
                    "{msg}\n\nPress u to run `restic unlock` (removes stale locks only) \
                     and retry, or any other key to return to the snapshot list."
                )
            } else {
                format!("{msg}\n\nPress any key to return to the snapshot list.")
            };
            let para = Paragraph::new(body)
                .style(Style::new().fg(Color::Red))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title("Delete unavailable"));
            frame.render_widget(para, area);
        }
        Screen::ResticUnlocking => {
            let para = Paragraph::new("Running `restic unlock` — removing stale locks…")
                .block(Block::bordered().title("Unlocking repository"));
            frame.render_widget(para, area);
        }
        Screen::OpeningSnapshot => {
            let para = Paragraph::new("Opening snapshot — reading root tree…")
                .block(Block::bordered().title("Loading"));
            frame.render_widget(para, area);
        }
        Screen::LoadingDir => {
            let para = Paragraph::new("Loading directory…")
                .block(Block::bordered().title("Loading"));
            frame.render_widget(para, area);
        }
        Screen::LoadingFileDetails => {
            let para = Paragraph::new("Reading file details…")
                .block(Block::bordered().title("Loading"));
            frame.render_widget(para, area);
        }
        Screen::SnapshotContents => render_snapshot_contents(frame, app, area),
        Screen::FileDetails => render_file_details(frame, app, area),
        Screen::ShareUrl => render_share_url(frame, app, area),
        Screen::PassphraseInstancePrompt => {
            let banner = format!(
                "No config found at {}. Setting up a new instance.",
                app.paths.config.display(),
            );
            let [_top, banner_area, input_area, help_area, _bottom] = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(area);
            let banner_p = Paragraph::new(banner).style(Style::new().fg(Color::White));
            frame.render_widget(banner_p, banner_area);
            draw_input_field(frame, input_area, "Instance name", &app.passphrase_instance_input, false, true);
            let help_p = Paragraph::new("Lowercase letters, digits, and hyphens (max 32 chars).").style(Style::new().fg(Color::White));
            frame.render_widget(help_p, help_area);
        }
        Screen::AuthMethodChoice => render_auth_method_choice(frame, app, area),
        Screen::PassphraseSetup => render_passphrase_setup(frame, app, area),
        Screen::PassphraseUnlock => render_passphrase_unlock(frame, app, area),
        Screen::PassphraseDerivingKey => {
            let para = Paragraph::new("Deriving key…")
                .block(Block::bordered().title("Passphrase"));
            frame.render_widget(para, area);
        }
        Screen::SnapshotCompareSecond => render_compare_second(frame, app, area),
        Screen::SnapshotCompareLoading => render_compare_loading(frame, app, area),
        Screen::SnapshotCompareResults => render_compare_results(frame, app, area),
        Screen::Error(msg) => {
            let title = if app.error_is_fatal {
                "Error — fatal"
            } else {
                "Error"
            };
            let para = Paragraph::new(msg.as_str())
                .style(Style::new().fg(Color::Red))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(title));
            frame.render_widget(para, area);
        }
    }
}

fn selection_highlight() -> Style {
    Style::new()
        .bg(Color::Rgb(40, 40, 80))
        .add_modifier(Modifier::BOLD)
}

// Stash the outer rect of the currently-rendered scrollable area so the
// key handler can size PageUp/PageDown jumps and the mouse handler can
// translate clicks into row indices. `area` is the outer rect that gets
// `Block::bordered()`; the bordered interior is `area` shrunk by 1 on
// each side.
fn record_list_area(app: &mut App, area: Rect) {
    app.list_area = Some(area);
}

// Decorate a creation/edit screen title with the in-progress profile name so
// the user can always see which profile they are editing.
fn profile_title(base: &str, app: &App) -> String {
    let name = app.new_profile_name.value();
    if name.is_empty() {
        base.to_string()
    } else {
        format!("{base} — profile '{name}'")
    }
}

fn render_home(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = "Profiles";

    if app.config.profiles.is_empty() {
        let para = Paragraph::new("No profiles yet. Press 'n' to create one, 'q' to quit.")
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(title));
        frame.render_widget(para, area);
        return;
    }

    app.list_header_rows = 1;

    let rows: Vec<Row> = app
        .config
        .profiles
        .iter()
        .map(|(name, p)| Row::new([Cell::from(name.clone()), Cell::from(p.backend_kind().label())]))
        .collect();

    let header = Row::new(["Name", "Backend"])
        .style(Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let table = Table::new(rows, [Constraint::Length(24), Constraint::Fill(1)])
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(selection_highlight())
        .highlight_symbol(">> ")
        .highlight_spacing(HighlightSpacing::Always)
        .column_spacing(2);

    record_list_area(app, area);
    frame.render_stateful_widget(table, area, &mut app.profile_list_state);
}

fn render_backend_choice(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = BACKEND_ORDER
        .iter()
        .map(|k| ListItem::new(k.label()))
        .collect();

    let name = app.new_profile_name.value();
    let title = if name.is_empty() {
        "Choose backend".to_string()
    } else {
        format!("Choose backend for '{name}'")
    };

    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(selection_highlight())
        .highlight_symbol(">> ");

    record_list_area(app, area);
    frame.render_stateful_widget(list, area, &mut app.backend_list);
}

fn render_input(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &Input,
    masked: bool,
    help: &str,
) {
    let [_top, input_area, help_area, _bottom] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    draw_input_field(frame, input_area, title, input, masked, true);

    let help = Paragraph::new(help).style(Style::new().fg(Color::White));
    frame.render_widget(help, help_area);
}

fn render_grouped_input(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    fields: &[(&str, &Input, bool)],
    focus: usize,
    help: &str,
) {
    let outer = Block::bordered().title(title);
    let inner_area = outer.inner(area);
    frame.render_widget(outer, area);

    let mut constraints: Vec<Constraint> = fields.iter().map(|_| Constraint::Length(3)).collect();
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Fill(1));
    let areas = Layout::vertical(constraints).split(inner_area);

    for (i, (label, input, masked)) in fields.iter().enumerate() {
        draw_input_field(frame, areas[i], label, input, *masked, i == focus);
    }

    let help_para = Paragraph::new(help).style(Style::new().fg(Color::White));
    frame.render_widget(help_para, areas[fields.len()]);
}

fn render_keychain_checkbox(frame: &mut Frame, area: Rect, checked: bool, focused: bool) {
    let mark = if checked { "[x]" } else { "[ ]" };
    let style = if focused {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::White)
    };
    let text = format!(" {mark} Save passphrase to keychain");
    frame.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_input_field(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &Input,
    masked: bool,
    focused: bool,
) {
    let value = input.value();
    let display: String = if masked {
        "*".repeat(value.chars().count())
    } else {
        value.to_string()
    };

    // For masked fields every char is a single column, so cursor and scroll
    // are simple codepoint math. For non-masked fields use the Input's own
    // grapheme-aware visual computation.
    let inner_width = area.width.saturating_sub(2);
    let (cursor_col, scroll) = if masked {
        let cur = input.cursor();
        let w = inner_width as usize;
        let scroll = if w == 0 { 0 } else { cur.saturating_sub(w - 1) };
        (cur, scroll)
    } else {
        let w = inner_width.max(1) as usize;
        let scroll = input.visual_scroll(w);
        (input.visual_cursor(), scroll)
    };

    let mut block = Block::bordered().title(title);
    if focused {
        block = block.border_style(Style::new().fg(Color::Yellow));
    }
    let para = Paragraph::new(display)
        .scroll((0, scroll as u16))
        .block(block);
    frame.render_widget(para, area);

    if focused {
        let visible_col = cursor_col.saturating_sub(scroll) as u16;
        frame.set_cursor_position((area.x + 1 + visible_col, area.y + 1));
    }
}

fn render_rest_config(frame: &mut Frame, app: &App, area: Rect) {
    let title = profile_title("REST backend", app);
    let fields = [
        ("URL (required)", &app.rest_url, false),
        ("Username (optional)", &app.rest_user, false),
        ("Password (optional)", &app.rest_password, true),
    ];
    render_grouped_input(
        frame,
        area,
        &title,
        &fields,
        app.field_focus,
        "Anonymous server? Leave user/password blank.",
    );
}

fn render_s3_location(frame: &mut Frame, app: &App, area: Rect) {
    let title = profile_title("S3 location", app);
    let fields = [
        ("Endpoint (optional)", &app.s3_endpoint, false),
        ("Bucket (required)", &app.s3_bucket, false),
        (
            "Region (optional, defaults to us-east-1)",
            &app.s3_region,
            false,
        ),
        ("Path within bucket (optional)", &app.s3_root, false),
    ];
    render_grouped_input(
        frame,
        area,
        &title,
        &fields,
        app.field_focus,
        "Leave Endpoint blank for AWS. Backblaze B2 uses region 'auto'. MinIO/rclone: http://127.0.0.1:8333",
    );
}

fn render_s3_credentials(frame: &mut Frame, app: &App, area: Rect) {
    let title = profile_title("S3 credentials", app);
    let fields = [
        ("Access key ID", &app.s3_access_key, false),
        ("Secret access key", &app.s3_secret_key, true),
    ];
    render_grouped_input(
        frame,
        area,
        &title,
        &fields,
        app.field_focus,
        "AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY equivalents.",
    );
}

fn render_snapshots(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_snapshot_indices();
    let total = app.snapshots.len();
    let title = match &app.snapshot_filter {
        None => format!("Snapshots ({total})"),
        Some(f) => format!(
            "Snapshots ({} of {}, {}={})",
            visible.len(),
            total,
            f.kind().label(),
            f.value()
        ),
    };

    record_list_area(app, area);
    app.list_header_rows = 1;
    render_snapshot_picker(frame, area, &title, &app.snapshots, &visible, &mut app.list_state);
}

// Shared picker rendering for the Snapshots screen and the two compare-flow
// pick screens. Indices are absolute into `snapshots`; the picker's own
// `TableState` is positional within `visible`.
fn render_snapshot_picker(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    snapshots: &[crate::repo::SnapshotRow],
    visible: &[usize],
    state: &mut TableState,
) {
    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| &snapshots[i])
        .map(|s| {
            let tags = if s.tags.is_empty() {
                String::new()
            } else {
                format!("[{}]", s.tags.join(","))
            };
            Row::new([
                Cell::from(short_snap_id(&s.id)),
                Cell::from(s.time.as_str()),
                Cell::from(s.host.as_str()),
                Cell::from(tags),
                Cell::from(s.paths.join(",")),
            ])
        })
        .collect();

    let header = Row::new(["ID", "Time", "Host", "Tags", "Paths"])
        .style(Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(19),
            Constraint::Length(20),
            Constraint::Length(20),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .block(Block::bordered().title(title))
    .row_highlight_style(selection_highlight())
    .highlight_symbol(">> ")
    .highlight_spacing(HighlightSpacing::Always)
    .column_spacing(2);

    frame.render_stateful_widget(table, area, state);
}

fn render_compare_second(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.compare_second_visible_indices();
    let scope = if app.compare_only_related { "related" } else { "all" };
    let first_short = app
        .compare_first_id
        .as_deref()
        .map(short_snap_id)
        .unwrap_or("?");
    let title = format!(
        "Compare {first_short}.. — pick SECOND snapshot ({}, {scope})",
        visible.len()
    );
    record_list_area(app, area);
    app.list_header_rows = 1;
    render_snapshot_picker(
        frame,
        area,
        &title,
        &app.snapshots,
        &visible,
        &mut app.compare_picker_state,
    );
}

fn render_compare_loading(frame: &mut Frame, app: &App, area: Rect) {
    let first = app.compare_first_id.as_deref().map(short_snap_id).unwrap_or("?");
    let second = app.compare_second_id.as_deref().map(short_snap_id).unwrap_or("?");
    let body = format!("Running `restic diff {first}..{second} --json`…");
    let para = Paragraph::new(body).block(Block::bordered().title("Computing diff"));
    frame.render_widget(para, area);
}

fn render_compare_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let first = app.compare_first_id.as_deref().map(short_snap_id).unwrap_or("?");
    let second = app.compare_second_id.as_deref().map(short_snap_id).unwrap_or("?");
    let title = format!("Diff {first}..{second}");

    let outer = Block::bordered().title(title);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let [header, body] = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(inner);

    let (summary_text, rows) = match &app.compare_results {
        Some((sum, changes)) => {
            let text = format!(
                "+{} files / -{} / M{}  |  +{} / -{}  ({} change{})",
                sum.added_files,
                sum.removed_files,
                sum.changed_files,
                human_size(sum.added_bytes),
                human_size(sum.removed_bytes),
                changes.len(),
                if changes.len() == 1 { "" } else { "s" },
            );
            let rows: Vec<Row> = changes
                .iter()
                .map(|c| Row::new([
                    Cell::from(c.modifier.as_char().to_string()),
                    Cell::from(c.path.clone()),
                ]))
                .collect();
            (text, rows)
        }
        None => ("(no diff loaded)".to_string(), Vec::new()),
    };

    let summary_para = Paragraph::new(summary_text)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title("Summary"));
    frame.render_widget(summary_para, header);

    if rows.is_empty() {
        let para = Paragraph::new("No file-level changes between these snapshots.")
            .style(Style::new().fg(Color::DarkGray))
            .block(Block::bordered().title("Changes"));
        frame.render_widget(para, body);
        return;
    }

    let count = rows.len();
    app.list_header_rows = 1;

    let col_header = Row::new(["M", "Path"])
        .style(Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let table = Table::new(rows, [Constraint::Length(1), Constraint::Fill(1)])
        .header(col_header)
        .block(Block::bordered().title(format!("Changes ({count})")))
        .row_highlight_style(selection_highlight())
        .highlight_symbol(">> ")
        .highlight_spacing(HighlightSpacing::Always)
        .column_spacing(2);

    record_list_area(app, body);
    frame.render_stateful_widget(table, body, &mut app.compare_results_state);
}

fn render_filter_dim(frame: &mut Frame, app: &mut App, area: Rect) {
    let entries = filter_dim_entries(app.snapshot_filter.is_some());
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| ListItem::new(e.label()))
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title("Filter snapshots by"))
        .highlight_style(selection_highlight())
        .highlight_symbol(">> ");
    record_list_area(app, area);
    frame.render_stateful_widget(list, area, &mut app.filter_picker_state);
}

fn render_filter_value(frame: &mut Frame, app: &mut App, area: Rect) {
    record_list_area(app, area);
    let kind_label = app
        .filter_pending_kind
        .map(|k| k.label())
        .unwrap_or("value");
    let title = format!("Pick {} ({})", kind_label, app.filter_values.len());
    let items: Vec<ListItem> = app
        .filter_values
        .iter()
        .map(|v| ListItem::new(v.as_str()))
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(selection_highlight())
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.filter_picker_state);
}

fn render_snapshot_contents(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.browse_stack.is_empty() {
        let para = Paragraph::new("No content loaded.")
            .block(Block::bordered().title("Snapshot contents"));
        frame.render_widget(para, area);
        return;
    }

    let path = app
        .browse_stack
        .iter()
        .skip(1)
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let path_display = if path.is_empty() { "/".to_string() } else { format!("/{path}") };
    let title = format!(
        "Snap {} {}",
        short_snap_id(&app.browse_snapshot_id),
        path_display,
    );

    app.list_header_rows = 1;

    let frame_idx = app.browse_stack.len() - 1;
    let top = &app.browse_stack[frame_idx];
    let rows: Vec<Row> = top
        .items
        .iter()
        .map(|row| {
            if matches!(row.kind, ContentKind::Parent) {
                return Row::new(["", "..", "", ""]);
            }
            let kind_char = match row.kind {
                ContentKind::Dir => "d",
                ContentKind::File => "-",
                ContentKind::Symlink => "l",
                ContentKind::Other => "?",
                ContentKind::Parent => "^",
            };
            let display_name = if matches!(row.kind, ContentKind::Dir) {
                format!("{}/", row.name)
            } else {
                row.name.clone()
            };
            let size_col = if matches!(row.kind, ContentKind::File) {
                human_size(row.size)
            } else {
                String::new()
            };
            Row::new([
                Cell::from(kind_char),
                Cell::from(display_name),
                Cell::from(Line::from(size_col).alignment(Alignment::Right)),
                Cell::from(row.mtime.clone()),
            ])
        })
        .collect();

    let header = Row::new([
        Cell::from("T"),
        Cell::from("Name"),
        Cell::from(Line::from("Size").alignment(Alignment::Right)),
        Cell::from("Modified"),
    ])
        .style(Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(19),
        ],
    )
    .header(header)
    .block(Block::bordered().title(title))
    .row_highlight_style(selection_highlight())
    .highlight_symbol(">> ")
    .highlight_spacing(HighlightSpacing::Always)
    .column_spacing(2);

    record_list_area(app, area);
    let top_mut = &mut app.browse_stack[frame_idx];
    frame.render_stateful_widget(table, area, &mut top_mut.table_state);
}

fn render_file_details(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.file_details.as_ref() else {
        let para = Paragraph::new("(no file selected)")
            .block(Block::bordered().title("File details"));
        frame.render_widget(para, area);
        return;
    };

    let mut lines = String::new();
    lines.push_str(&format!("Name:  {}\n", d.name));
    lines.push_str(&format!("Path:  {}\n", d.full_path));
    lines.push_str(&format!("Type:  {}\n", d.kind_label));
    lines.push_str(&format!("Size:  {} ({} bytes)\n", human_size(d.size), d.size));
    if let Some(m) = d.mode {
        lines.push_str(&format!("Mode:  0{m:o}\n"));
    }
    if let Some(t) = &d.mtime {
        lines.push_str(&format!("mtime: {t}\n"));
    }
    if let Some(t) = &d.atime {
        lines.push_str(&format!("atime: {t}\n"));
    }
    if let Some(t) = &d.ctime {
        lines.push_str(&format!("ctime: {t}\n"));
    }
    let owner = match (d.user.as_deref(), d.uid) {
        (Some(u), Some(n)) => format!("{u} ({n})"),
        (Some(u), None) => u.to_string(),
        (None, Some(n)) => n.to_string(),
        (None, None) => String::new(),
    };
    let group = match (d.group.as_deref(), d.gid) {
        (Some(g), Some(n)) => format!("{g} ({n})"),
        (Some(g), None) => g.to_string(),
        (None, Some(n)) => n.to_string(),
        (None, None) => String::new(),
    };
    if !owner.is_empty() || !group.is_empty() {
        lines.push_str(&format!("Owner: {owner} / {group}\n"));
    }
    if let Some(t) = &d.linktarget {
        lines.push_str(&format!("Link target: {t}\n"));
    }
    if !d.content_hashes.is_empty() {
        // Cap the displayed list so deeply-chunked files don't push the rest
        // of the metadata off-screen. Show the first MAX hashes; the count
        // in the header already tells the reader how many are hidden.
        const MAX: usize = 10;
        lines.push_str(&format!(
            "\nContent blob SHA-256 ({} chunk{}):\n",
            d.content_hashes.len(),
            if d.content_hashes.len() == 1 { "" } else { "s" },
        ));
        for h in d.content_hashes.iter().take(MAX) {
            lines.push_str("  ");
            lines.push_str(h);
            lines.push('\n');
        }
        if d.content_hashes.len() > MAX {
            lines.push_str(&format!(
                "  … ({} more)\n",
                d.content_hashes.len() - MAX
            ));
        }
    } else if matches!(d.kind, ContentKind::File) {
        lines.push_str("\n(empty file — no content blobs)\n");
    }

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.file_details_scroll, 0))
        .block(
            Block::bordered().title(format!("File details — {}", short_path(&d.full_path))),
        );
    record_list_area(app, area);
    frame.render_widget(para, area);
}

fn render_auth_method_choice(frame: &mut Frame, app: &mut App, area: Rect) {
    let items = vec![
        ListItem::new("Use passphrase from keychain"),
        ListItem::new("Enter passphrase manually"),
    ];
    let list = List::new(items)
        .block(Block::bordered().title("Choose unlock method"))
        .highlight_style(selection_highlight())
        .highlight_symbol(">> ");
    record_list_area(app, area);
    frame.render_stateful_widget(list, area, &mut app.auth_method_list);
}

fn render_passphrase_setup(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.passphrase_instance_value.is_empty() {
        "Set passphrase".to_string()
    } else {
        format!("Set passphrase \u{2014} {}", app.passphrase_instance_value)
    };
    let help = app
        .passphrase_error
        .as_deref()
        .unwrap_or("Min 12 chars, requires lowercase, uppercase, digit, and special character.");

    if app.keychain_enabled() {
        let outer = Block::bordered().title(title);
        let inner_area = outer.inner(area);
        frame.render_widget(outer, area);

        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner_area);

        draw_input_field(frame, areas[0], "Passphrase", &app.passphrase_input, true, app.field_focus == 0);
        draw_input_field(frame, areas[1], "Confirm passphrase", &app.passphrase_confirm, true, app.field_focus == 1);
        render_keychain_checkbox(frame, areas[2], app.save_to_keychain, app.field_focus == 2);
        let help_para = Paragraph::new(help).style(Style::new().fg(Color::White));
        frame.render_widget(help_para, areas[3]);
    } else {
        let fields = [
            ("Passphrase", &app.passphrase_input, true),
            ("Confirm passphrase", &app.passphrase_confirm, true),
        ];
        render_grouped_input(frame, area, &title, &fields, app.field_focus, help);
    }
}

fn render_passphrase_unlock(frame: &mut Frame, app: &App, area: Rect) {
    let meta = app.config.passphrase.as_ref();
    let title = match meta.map(|m| m.instance.as_str()).filter(|s| !s.is_empty()) {
        Some(inst) => format!("Unlock \u{2014} {inst}"),
        None => "Unlock".to_string(),
    };
    let help = if let Some(err) = &app.passphrase_error {
        err.clone()
    } else if let Some(sig) = meta.map(|m| m.instance_sig.as_str()).filter(|s| !s.is_empty()) {
        format!("Signature: {sig}")
    } else {
        "Enter the passphrase to decrypt the config.".to_string()
    };

    if app.keychain_enabled() {
        let outer = Block::bordered().title(title);
        let inner_area = outer.inner(area);
        frame.render_widget(outer, area);

        let areas = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner_area);

        draw_input_field(frame, areas[1], "Passphrase", &app.passphrase_input, true, app.field_focus == 0);
        render_keychain_checkbox(frame, areas[2], app.save_to_keychain, app.field_focus == 1);
        let help_para = Paragraph::new(help).style(Style::new().fg(Color::White));
        frame.render_widget(help_para, areas[3]);
    } else {
        render_input(frame, area, &title, &app.passphrase_input, true, &help);
    }
}

fn render_share_url(frame: &mut Frame, app: &mut App, area: Rect) {
    let port = app.server_port;
    let target_label = app
        .share_target
        .as_ref()
        .map(|t| short_path(&t.display_path))
        .unwrap_or_else(|| "(no file)".to_string());

    let mut lines = String::new();
    let running = app.share_handle.is_some();
    if running {
        lines.push_str(&format!("Server: listening on localhost:{port}\n"));
    } else {
        lines.push_str("Server: not running\n");
    }
    lines.push('\n');

    match (&app.share_short_url, &app.share_url) {
        (Some(short), Some(long)) => {
            lines.push_str("Short URL (302 redirects to the long URL):\n");
            lines.push_str(short);
            lines.push_str("\n\n");
            lines.push_str("Long URL:\n");
            lines.push_str(long);
            lines.push_str("\n\n");
        }
        _ => {
            lines.push_str("URL: (none yet — start the server with `s`)\n\n");
        }
    }

    if let Some(exp) = app.share_exp_unix {
        // Absolute timestamp in local time — doesn't need re-rendering as
        // the clock advances. `exp` is a u64 unix-seconds; convert via i64.
        let when = jiff::Timestamp::from_second(exp as i64)
            .and_then(|t| t.in_tz("UTC").map(|z| z.strftime("%Y-%m-%d %H:%M:%S UTC").to_string()))
            .unwrap_or_else(|_| format!("unix {exp}"));
        let local = jiff::Timestamp::from_second(exp as i64)
            .map(|t| t.to_zoned(jiff::tz::TimeZone::system()))
            .map(|z| z.strftime("%Y-%m-%d %H:%M:%S %Z").to_string())
            .unwrap_or_default();
        if local.is_empty() || local == when {
            lines.push_str(&format!("Expires: {when}\n"));
        } else {
            lines.push_str(&format!("Expires: {local}  ({when})\n"));
        }
    }

    if let Some(err) = &app.share_error {
        lines.push_str(&format!("\nError: {err}\n"));
    }

    lines.push_str(
        "\nServer is bound to this file only. Leaving this screen stops it.",
    );

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(format!("Share — {target_label}")));
    record_list_area(app, area);
    frame.render_widget(para, area);
}

// Truncate a path for a title — keep the tail since the basename is usually
// the most identifying piece. Returns the full path if it already fits.
// Operates on character counts (not bytes) so multi-byte filenames don't
// panic from slicing inside a codepoint.
fn short_path(p: &str) -> String {
    const MAX_CHARS: usize = 60;
    let nchars = p.chars().count();
    if nchars <= MAX_CHARS {
        return p.to_string();
    }
    // Keep the trailing (MAX_CHARS - 1) chars; the leading "…" takes the
    // remaining char-width, so total displayed width stays at MAX_CHARS.
    let skip = nchars - (MAX_CHARS - 1);
    let byte_offset = p
        .char_indices()
        .nth(skip)
        .map(|(i, _)| i)
        .unwrap_or(p.len());
    format!("…{}", &p[byte_offset..])
}

fn short_snap_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
    &id[..end]
}

// 1024-based suffixes matching `ls -h` (e.g. "4.2K", "1.7M"). Bytes are shown
// raw; larger values keep one decimal place.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, UNITS[i])
}

fn render_snapshot_delete_confirm(frame: &mut Frame, app: &mut App, area: Rect) {
    let id = app.delete_target.as_deref().unwrap_or("(unknown)");
    let info = app.delete_info.as_ref();
    let paths = info
        .map(|i| {
            if i.paths.is_empty() {
                "(no paths)".to_string()
            } else {
                i.paths.join(", ")
            }
        })
        .unwrap_or_else(|| "(unknown)".into());
    let host = info.map(|i| i.hostname.as_str()).unwrap_or("?");

    let outer = Block::bordered().title("Confirm snapshot delete");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let [header, body] = Layout::vertical([Constraint::Length(8), Constraint::Fill(1)]).areas(inner);

    let tags = info
        .filter(|i| !i.tags.is_empty())
        .map(|i| i.tags.join(", "));
    let mut summary = format!("Delete snapshot {id}?\n\nHost: {host}\nPaths: {paths}");
    if let Some(tags) = tags {
        summary.push_str(&format!("\nTags: {tags}"));
    }
    if let Some(sum) = app.delete_details_parsed.as_ref().and_then(|p| p.summary.as_ref()) {
        let files = sum
            .total_files_processed
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into());
        let bytes = sum
            .total_bytes_processed
            .map(human_size)
            .unwrap_or_else(|| "?".into());
        summary.push_str(&format!("\n\nContents: {files} files, {bytes}"));
    }
    summary.push_str(&format!(
        "\n\nThis runs `restic forget {id}` (no prune)."
    ));
    let header_para = Paragraph::new(summary)
        .style(Style::new().fg(Color::Yellow))
        .wrap(Wrap { trim: false });
    frame.render_widget(header_para, header);

    if app.delete_show_json {
        let raw = app.delete_details_raw.as_deref().unwrap_or("(no raw JSON)");
        let raw_para = Paragraph::new(raw)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title("Raw `restic snapshots --json`"));
        frame.render_widget(raw_para, body);
        return;
    }

    let Some(preview) = app.delete_root_listing.as_ref() else {
        let para = Paragraph::new("(no preview)")
            .style(Style::new().fg(Color::DarkGray))
            .block(Block::bordered().title("Contents"));
        frame.render_widget(para, body);
        return;
    };
    if preview.entries.is_empty() {
        let para = Paragraph::new("(snapshot is empty)")
            .style(Style::new().fg(Color::DarkGray))
            .block(Block::bordered().title("Contents"));
        frame.render_widget(para, body);
        return;
    }

    let mut rows: Vec<Row> = preview
        .entries
        .iter()
        .map(|row| {
            let kind_char = match row.kind {
                ContentKind::Dir => "d",
                ContentKind::File => "-",
                ContentKind::Symlink => "l",
                ContentKind::Other => "?",
                ContentKind::Parent => "^",
            };
            let display_path = if matches!(row.kind, ContentKind::Dir) {
                format!("{}/", row.path)
            } else {
                row.path.clone()
            };
            let size_col = if matches!(row.kind, ContentKind::File) {
                human_size(row.size)
            } else {
                String::new()
            };
            Row::new([
                Cell::from(kind_char),
                Cell::from(Line::from(size_col).alignment(Alignment::Right)),
                Cell::from(display_path),
            ])
        })
        .collect();
    if preview.truncated {
        rows.push(
            Row::new([Cell::from(""), Cell::from(""), Cell::from("…load more")])
                .style(Style::new().fg(Color::DarkGray)),
        );
    }
    let title = if preview.truncated {
        format!("Contents (first {} entries)", preview.entries.len())
    } else {
        format!("Contents ({} entries)", preview.entries.len())
    };

    let col_header = Row::new([
        Cell::from("T"),
        Cell::from(Line::from("Size").alignment(Alignment::Right)),
        Cell::from("Path"),
    ])
    .style(Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(10),
            Constraint::Fill(1),
        ],
    )
    .header(col_header)
    .block(Block::bordered().title(title))
    .row_highlight_style(Style::new().bg(Color::DarkGray))
    .column_spacing(2);

    app.list_header_rows = 1;
    frame.render_stateful_widget(table, body, &mut app.delete_preview_state);
}

#[cfg(test)]
mod tests {
    use super::{human_size, short_path};

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 K");
        assert_eq!(human_size(1536), "1.5 K");
        assert_eq!(human_size(1024 * 1024), "1.0 M");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 G");
    }

    #[test]
    fn short_path_passes_short_inputs_through() {
        let s = "/home/x/foo.txt";
        assert_eq!(short_path(s), s);
    }

    #[test]
    fn short_path_truncates_long_paths_from_the_head() {
        let s: String = std::iter::repeat_n('a', 120).collect();
        let out = short_path(&s);
        assert_eq!(out.chars().count(), 60);
        assert!(out.starts_with('…'));
    }

    #[test]
    fn short_path_handles_multibyte_chars_without_panicking() {
        // 80 emoji (4 bytes each in UTF-8) — the previous byte-indexed slice
        // would land mid-codepoint and panic.
        let s: String = std::iter::repeat_n('🦀', 80).collect();
        let out = short_path(&s);
        assert_eq!(out.chars().count(), 60);
        assert!(out.starts_with('…'));
    }
}

use super::input::{Panel, UiState};
use super::runtime::Snapshot;
use super::{border_glyphs, status_bar};
use anyhow::Result;
use std::io::Write;
use std::time::Instant;
use unicode_width::UnicodeWidthChar;
use vt100::{Cell, Color, Screen};

const BAR: &str = "\x1b[48;2;33;36;37;38;2;220;224;220m";
const ACTIVE: &str = "\x1b[38;2;167;230;54m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Layout {
    pub agent_rows: u16,
    pub agent_cols: u16,
    rows: u16,
    cols: u16,
}

impl Layout {
    pub fn new(size: libc::winsize, _state: &UiState) -> Self {
        Self {
            agent_rows: size.ws_row - 4,
            agent_cols: size.ws_col - 2,
            rows: size.ws_row,
            cols: size.ws_col,
        }
    }

    fn floating_rect(self) -> (u16, u16, u16, u16) {
        let width = (self.cols - self.cols / 5)
            .max(76)
            .min(self.cols.saturating_sub(4));
        let height = (self.agent_rows - self.agent_rows / 4)
            .max(16)
            .min(self.agent_rows);
        let x = (self.cols - width) / 2 + 1;
        let y = 3 + (self.agent_rows - height) / 2;
        (x, y, width, height)
    }
}

fn move_to(buf: &mut Vec<u8>, row: u16, col: u16) {
    write!(buf, "\x1b[{row};{col}H").unwrap();
}

fn print_clipped(buf: &mut Vec<u8>, value: &str, width: u16) {
    let mut used = 0;
    for ch in value.chars() {
        let cell_width = if ch.is_control() {
            1
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        if used + cell_width > usize::from(width) {
            break;
        }
        used += cell_width;
        if ch.is_control() {
            buf.push(b' ');
        } else {
            write!(buf, "{ch}").unwrap();
        }
    }
}

fn bar_line(buf: &mut Vec<u8>, row: u16, cols: u16, text: &str) {
    move_to(buf, row, 1);
    buf.extend_from_slice(BAR.as_bytes());
    buf.extend_from_slice(" ".repeat(cols as usize).as_bytes());
    move_to(buf, row, 1);
    print_clipped(buf, text, cols);
    buf.extend_from_slice(b"\x1b[0m");
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl From<&Cell> for CellStyle {
    fn from(cell: &Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }
}

fn color_code(buf: &mut Vec<u8>, color: Color, foreground: bool) {
    let base = if foreground { 38 } else { 48 };
    match color {
        Color::Default => write!(buf, ";{}", if foreground { 39 } else { 49 }).unwrap(),
        Color::Idx(value) => write!(buf, ";{base};5;{value}").unwrap(),
        Color::Rgb(red, green, blue) => {
            write!(buf, ";{base};2;{red};{green};{blue}").unwrap();
        }
    }
}

fn set_cell_style(buf: &mut Vec<u8>, style: CellStyle) {
    buf.extend_from_slice(b"\x1b[0");
    if style.bold {
        buf.extend_from_slice(b";1");
    }
    if style.dim {
        buf.extend_from_slice(b";2");
    }
    if style.italic {
        buf.extend_from_slice(b";3");
    }
    if style.underline {
        buf.extend_from_slice(b";4");
    }
    if style.inverse {
        buf.extend_from_slice(b";7");
    }
    color_code(buf, style.fg, true);
    color_code(buf, style.bg, false);
    buf.push(b'm');
}

fn draw_agent_row(buf: &mut Vec<u8>, row: u16, width: u16, screen: &Screen) {
    move_to(buf, row + 3, 2);
    buf.extend_from_slice(b"\x1b[0m");
    let mut prev_style = None;
    for col in 0..width {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let style = CellStyle::from(cell);
        if prev_style != Some(style) {
            set_cell_style(buf, style);
            prev_style = Some(style);
        }
        if cell.has_contents() {
            buf.extend_from_slice(cell.contents().as_bytes());
        } else {
            buf.push(b' ');
        }
    }
    buf.extend_from_slice(b"\x1b[0m");
}

fn wrap_log_lines(lines: &[String], width: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    for line in lines {
        let mut current = String::new();
        let mut used = 0;
        for ch in line.chars() {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
            if used + char_width > width && !current.is_empty() {
                wrapped.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(if ch.is_control() { ' ' } else { ch });
            used += char_width;
        }
        wrapped.push(current);
    }
    wrapped
}

fn panel_lines(snapshot: &Snapshot, panel: Panel, started: Instant, width: usize) -> Vec<String> {
    let (hits, effects, denied, failed) = snapshot.file_totals();
    let (allowed, net_denied, net_failed) = snapshot.network_totals();
    let record = snapshot.record.as_ref();
    match panel {
        Panel::Overview => {
            let mut lines = vec![
                format!(
                    "State       {}",
                    record.map_or("starting", |run| run.state.as_str())
                ),
                format!("Elapsed     {}s", started.elapsed().as_secs()),
                format!(
                    "Agent       {}",
                    record.map_or("pending", |run| run.agent.as_str())
                ),
                String::new(),
                "FILESYSTEM".into(),
                format!("  {effects} effects   {denied} denied"),
                format!("  {hits} hits      {failed} failed"),
                String::new(),
                "NETWORK".into(),
                format!("  {allowed} allowed   {net_denied} denied"),
                format!("  {net_failed} failed"),
                String::new(),
                "WORKSPACE".into(),
                record
                    .and_then(|run| run.workspace.as_ref())
                    .map_or("pending".into(), |path| path.display().to_string()),
            ];
            if let Some(image) = &snapshot.image {
                let total_files = image
                    .totals
                    .map_or_else(|| "?".into(), |t| t.files.to_string());
                let total_bytes = image
                    .totals
                    .map_or_else(|| "?".into(), |t| t.bytes.to_string());
                let mut image_lines = vec![
                    "IMAGE CONTENT (this run)".into(),
                    image.image.clone(),
                    format!("  {:<13} {:>10}  {:>16}", "", "Files", "Bytes"),
                    format!(
                        "  {:<13} {:>10}  {:>16}",
                        "Cached", image.cached_files, image.cached_bytes
                    ),
                    format!(
                        "  {:<13} {:>10}  {:>16}",
                        "Transferred", image.downloaded_files, image.downloaded_bytes
                    ),
                    format!("  {:<13} {:>10}  {:>16}", "Total", total_files, total_bytes),
                    "  Cached: bytes served from disk/memory cache, including repeats".into(),
                    "  Transferred: verified bytes received from the image server".into(),
                    "  Total: regular files and logical bytes in the whole image".into(),
                    "  Files are distinct paths; partial reads count; kernel cache hits excluded"
                        .into(),
                    String::new(),
                ];
                image_lines.extend(lines.drain(4..));
                lines.extend(image_lines);
            }
            lines
        }
        Panel::Files => {
            let mut lines = vec![
                format!("{effects} effects   {denied} denied   {failed} failed"),
                String::new(),
            ];
            if let Some(filesystem) = &snapshot.filesystem {
                lines.push(format!(
                    "{} paths   {} observations omitted",
                    filesystem.paths.len(),
                    filesystem.overflow_hits
                ));
                lines.push(
                    "Observed by OverlayFS; paths outside this boundary are not counted".into(),
                );
                lines.push(String::new());
                let mut paths = filesystem.paths.iter().collect::<Vec<_>>();
                paths.sort_by(|left, right| {
                    let priority = |operations: &std::collections::BTreeMap<
                        _,
                        pvisor_core::operation::PathOperationCounters,
                    >| {
                        operations.values().fold((0u64, 0u64, 0u64), |sum, counts| {
                            (
                                sum.0 + counts.denied,
                                sum.1 + counts.effects,
                                sum.2 + counts.hits,
                            )
                        })
                    };
                    priority(right.1)
                        .cmp(&priority(left.1))
                        .then_with(|| left.0.cmp(right.0))
                });
                for (path, operations) in paths {
                    for (operation, counts) in operations {
                        lines.push(format!(
                            "{path}  {operation}: {} hits, {} denied, {} effects, {} failed",
                            counts.hits, counts.denied, counts.effects, counts.failed
                        ));
                    }
                }
            }
            lines
        }
        Panel::Network => {
            let mut lines = vec![
                format!("Allowed     {allowed}"),
                format!("Denied      {net_denied}"),
                format!("Failed      {net_failed}"),
                String::new(),
                "BOUNDARY".into(),
                record
                    .and_then(|run| run.network_interception.as_ref())
                    .map_or("pending".into(), |value| {
                        format!("{:?} / {:?}", value.driver, value.strength)
                    }),
                String::new(),
                "POLICY".into(),
                record.map_or("pending".into(), |run| run.network.to_string()),
                String::new(),
                "DESTINATIONS REACHING OVERLAYNET".into(),
            ];
            if let Some(network) = snapshot.network.as_ref().and_then(|value| {
                serde_json::from_value::<pvisor_overlaynet::InterceptionSnapshot>(value.clone())
                    .ok()
            }) {
                let mut targets = network.targets.iter().collect::<Vec<_>>();
                targets.sort_by(|left, right| {
                    (right.1.denied, right.1.failed, right.1.allowed)
                        .cmp(&(left.1.denied, left.1.failed, left.1.allowed))
                        .then_with(|| left.0.cmp(right.0))
                });
                for (target, counts) in targets {
                    lines.push(format!(
                        "{target}: allow {}  deny {}  fail {}",
                        counts.allowed, counts.denied, counts.failed
                    ));
                }
                if network.target_overflow > 0 {
                    lines.push(format!(
                        "{} events omitted by destination limit",
                        network.target_overflow
                    ));
                }
            }
            lines
        }
        Panel::Run => vec![
            "JOB ID".into(),
            record.map_or("pending".into(), |run| run.run_id.clone()),
            String::new(),
            "COMMAND".into(),
            record.map_or("pending".into(), |run| run.command.join(" ")),
            String::new(),
            "STORAGE".into(),
            snapshot
                .stage
                .as_ref()
                .map_or("pending".into(), |path| path.display().to_string()),
        ],
        Panel::Log => {
            if snapshot.log.is_empty() {
                vec!["No pVisor diagnostics yet.".into()]
            } else {
                wrap_log_lines(&snapshot.log, width)
            }
        }
        Panel::Permissions => permission_lines(snapshot, 0, false),
        Panel::Keys => super::input::help_lines(),
    }
}

fn permission_lines(snapshot: &Snapshot, selected: usize, confirm: bool) -> Vec<String> {
    let mut lines = vec![
        if confirm {
            "Press x again to forget; broader rules may apply."
        } else {
            "j/k Select decision   x Forget (asks for confirmation)"
        }
        .into(),
        "Saved decisions apply to ask rules; explicit deny wins.".into(),
    ];
    if let Some(overlay) = snapshot.record.as_ref().and_then(|r| r.overlay.as_ref()) {
        lines.push(format!(
            "File rules: {} deny / {} ask / {} warn",
            overlay.access_policy.deny().len(),
            overlay.access_policy.ask().len(),
            overlay.access_policy.warn().len()
        ));
        lines.push(format!("Rule root: {}", overlay.target.display()));
    }
    lines.push("DECISIONS (session > workspace > user)".into());
    if snapshot.audit_rules.is_empty() {
        lines.push("No saved decisions. Matching sensitive files still ask.".into());
    } else {
        lines.extend(
            snapshot
                .audit_rules
                .iter()
                .enumerate()
                .skip(selected)
                .map(|(i, line)| format!("{} {line}", if i == selected { ">" } else { " " })),
        );
    }
    lines
}

fn floating_panel(
    buf: &mut Vec<u8>,
    layout: Layout,
    state: &mut UiState,
    snapshot: &Snapshot,
    started: Instant,
) {
    let (x, y, width, height) = layout.floating_rect();
    let title = state.panel.title();
    let horizontal = border_glyphs::HORIZONTAL;
    move_to(buf, y, x);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(border_glyphs::TOP_LEFT.as_bytes());
    for _ in 0..width - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(border_glyphs::TOP_RIGHT.as_bytes());
    move_to(buf, y, x + 2);
    print_clipped(buf, &format!(" pVisor Review · {title} "), width - 4);

    let tabs = if width < 55 {
        "1 Overview  2 Files  3 Net  4 Job  5 Log  6 Perm"
    } else {
        "1 Overview   2 Files   3 Network   4 Job   5 Log   6 Permissions"
    };
    let lines = if state.panel == Panel::Permissions {
        permission_lines(snapshot, state.permission, state.forget_pending)
    } else {
        panel_lines(snapshot, state.panel, started, (width - 4) as usize)
    };
    let lines = wrap_log_lines(&lines, usize::from(width.saturating_sub(4)).max(1));
    state.page_rows = usize::from(height.saturating_sub(4)).max(1);
    state.max_scroll = lines.len().saturating_sub(state.page_rows);
    if state.panel == Panel::Permissions {
        state.max_scroll = snapshot.audit_rules.len().saturating_sub(1);
        state.scroll = 0;
    } else {
        state.scroll = state.scroll.min(state.max_scroll);
    }
    let first = state.scroll;
    let last = (first + state.page_rows).min(lines.len());
    for inner in 0..height - 2 {
        let row = y + inner + 1;
        move_to(buf, row, x);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::VERTICAL.as_bytes());
        buf.extend_from_slice(b"\x1b[48;2;15;19;16m");
        buf.extend_from_slice(" ".repeat((width - 2) as usize).as_bytes());
        move_to(buf, row, x + width - 1);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::VERTICAL.as_bytes());
        move_to(buf, row, x + 2);
        buf.extend_from_slice(if inner == 0 {
            b"\x1b[48;2;15;19;16;38;2;167;230;54m"
        } else {
            b"\x1b[48;2;15;19;16;38;2;220;224;220m"
        });
        let line = if inner == 0 {
            Some(tabs)
        } else if inner == 1 {
            None
        } else {
            lines
                .get((inner - 2) as usize + state.scroll)
                .map(String::as_str)
        };
        print_clipped(buf, line.unwrap_or(""), width - 4);
    }
    move_to(buf, y + height - 1, x);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(border_glyphs::BOTTOM_LEFT.as_bytes());
    for _ in 0..width - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(border_glyphs::BOTTOM_RIGHT.as_bytes());
    if state.panel != Panel::Permissions && !lines.is_empty() {
        move_to(buf, y + height - 1, x + 2);
        print_clipped(
            buf,
            &format!(
                " {}–{} / {}  ↑↓ Scroll · PgUp/PgDn · Home/End ",
                first + 1,
                last,
                lines.len()
            ),
            width - 4,
        );
    }
    buf.extend_from_slice(b"\x1b[0m");
}

fn boundary_label(snapshot: &Snapshot) -> String {
    let Some(run) = &snapshot.record else {
        return "Preparing runtime…".into();
    };
    let files = match &run.overlay {
        None => "Files: host paths",
        Some(overlay) if overlay.auto_apply => "Files: overlay (auto-apply)",
        Some(overlay) if overlay.auto_discard => "Files: overlay (discard on exit)",
        Some(_) => "Files: overlay (review to apply)",
    };
    let network = match &run.network_interception {
        Some(profile) if profile.is_enforcing() => "Net: enforced interception",
        Some(_) => "Net: cooperative proxy",
        None => "Net: see Review for policy",
    };
    format!("{files}  |  {network}")
}

fn audit_dialog(
    buf: &mut Vec<u8>,
    layout: Layout,
    request: &pvisor_core::audit::AuditRequest,
    prompt: &super::audit_ui::Prompt,
) {
    use super::audit_ui::{Lifetime, Scope, choice};
    use pvisor_core::audit::AuditKind;
    const BODY: &str = "\x1b[0;48;2;28;32;40;38;2;232;235;240m";
    const AMBER: &str = "\x1b[0;48;2;28;32;40;38;2;255;190;80m";
    const SELECTED: &str = "\x1b[1;48;2;255;190;80;38;2;24;28;34m";
    let width = layout.cols.saturating_sub(4).clamp(10, 88);
    let height = layout.rows.saturating_sub(2).min(16);
    let x = (layout.cols - width) / 2 + 1;
    let y = (layout.rows - height) / 2 + 1;
    let title = match request.kind {
        AuditKind::File => " FILE ACCESS PAUSED ",
        AuditKind::Network => " NETWORK ACCESS PAUSED ",
    };
    for row in 0..height {
        move_to(buf, y + row, x);
        buf.extend_from_slice(AMBER.as_bytes());
        let (left, right) = if row == 0 {
            ("╭", "╮")
        } else if row == height - 1 {
            ("╰", "╯")
        } else {
            ("│", "│")
        };
        write!(
            buf,
            "{left}{}{right}",
            if row == 0 || row == height - 1 {
                "─"
            } else {
                " "
            }
            .repeat((width - 2) as usize)
        )
        .unwrap();
    }
    let mut line = |row: u16, text: &str, style: &str| {
        move_to(buf, y + row, x + 2);
        buf.extend_from_slice(style.as_bytes());
        print_clipped(buf, text, width - 4);
    };
    line(0, title, SELECTED);
    if height < 14 || width < 56 {
        line(1, "Resize to review access", BODY);
        line(height - 2, "[ d Deny ]", SELECTED);
        return;
    }
    line(
        1,
        if request.kind == AuditKind::File {
            "Allow file access (not just reading)"
        } else {
            "This connection needs your permission"
        },
        BODY,
    );
    let label = request
        .scope
        .as_ref()
        .map(|scope| format!("[{}] {}", scope.view, request.target))
        .unwrap_or_else(|| request.target.clone());
    let target = wrap_log_lines(std::slice::from_ref(&label), (width - 4) as usize);
    for (index, text) in target.iter().take(2).enumerate() {
        line(2 + index as u16, text, AMBER);
    }
    line(
        4,
        if request.reason == "not-in-allowlist" {
            "No matching permission has been saved."
        } else {
            &request.reason
        },
        BODY,
    );
    line(
        5,
        if request.kind == AuditKind::Network {
            "Limited to this port and transport."
        } else {
            "Includes inspect, read, change, and delete in the view."
        },
        BODY,
    );
    line(
        6,
        if prompt.focus == 0 {
            "> ALLOW ACCESS TO   (Left / Right)"
        } else {
            "  ALLOW ACCESS TO"
        },
        AMBER,
    );
    line(
        8,
        if prompt.focus == 1 {
            "> REMEMBER FOR      (Left / Right)"
        } else {
            "  REMEMBER FOR"
        },
        AMBER,
    );
    let broad = prompt.lifetime == Lifetime::User && prompt.scope == Some(Scope::Suffix);
    line(
        10,
        if broad {
            "All workspaces: files with this suffix will be allowed."
        } else if prompt.lifetime == Lifetime::User {
            "Saved for this user, available across workspaces."
        } else if prompt.lifetime == Lifetime::Workspace {
            "Saved for future sessions in this workspace."
        } else {
            "Applies only to this session."
        },
        BODY,
    );
    line(
        height - 2,
        "Tab / Up / Down Move   Enter Confirm   d Deny",
        BODY,
    );
    let scope_options = match request.kind {
        AuditKind::File => [
            (b'1', "This file"),
            (b'2', "Same folder"),
            (b'3', "Same suffix"),
        ]
        .to_vec(),
        AuditKind::Network => [(b'1', "This target"), (b'2', "Host + subdomains")].to_vec(),
    };
    for (row, options, focus) in [
        (
            7,
            scope_options
                .into_iter()
                .filter_map(|(key, label)| {
                    choice(request, key).map(|(scope, _)| {
                        (
                            format!("{} {label}", key as char),
                            prompt.scope.unwrap_or(Scope::Exact) == scope,
                        )
                    })
                })
                .collect::<Vec<_>>(),
            0,
        ),
        (
            9,
            [
                ("s Session", Lifetime::Session),
                ("w Workspace", Lifetime::Workspace),
                ("u User", Lifetime::User),
            ]
            .into_iter()
            .map(|(label, lifetime)| (label.to_string(), lifetime == prompt.lifetime))
            .collect(),
            1,
        ),
        (
            height - 3,
            vec![
                ("Deny".into(), prompt.scope.is_none()),
                ("Allow & remember".into(), prompt.scope.is_some()),
            ],
            2,
        ),
    ] {
        move_to(buf, y + row, x + 2);
        let mut remaining = width - 4;
        for (label, selected) in options {
            let label = if width < 72 {
                label
                    .replace("This file", "File")
                    .replace("Same folder", "Folder")
                    .replace("Same suffix", "Suffix")
                    .replace("This target", "Target")
                    .replace("Host + subdomains", "Subdomains")
            } else {
                label
            };
            let text = format!(
                "{}[{} {}] ",
                if prompt.focus == focus && selected {
                    ">"
                } else {
                    " "
                },
                if selected { "●" } else { "○" },
                label
            );
            buf.extend_from_slice(if selected {
                SELECTED.as_bytes()
            } else {
                BODY.as_bytes()
            });
            print_clipped(buf, &text, remaining);
            remaining = remaining
                .saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()) as u16);
        }
    }
    buf.extend_from_slice(b"\x1b[0m");
}

pub(super) fn render(
    stdout: &mut impl Write,
    layout: Layout,
    state: &mut UiState,
    screen: &Screen,
    snapshot: &Snapshot,
    started: Instant,
) -> Result<()> {
    let mut buf = Vec::with_capacity(usize::from(layout.rows) * usize::from(layout.cols) * 3);
    buf.extend_from_slice(b"\x1b[?25l");
    let agent = snapshot
        .record
        .as_ref()
        .map_or("Agent", |run| run.agent.as_str());
    let workspace = snapshot
        .record
        .as_ref()
        .and_then(|run| run.workspace.as_ref())
        .map_or_else(
            || "Loading workspace…".into(),
            |path| path.display().to_string(),
        );
    bar_line(
        &mut buf,
        1,
        layout.cols,
        &format!(" pVisor  |  {agent}  |  {workspace}"),
    );

    move_to(&mut buf, 2, 1);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(border_glyphs::TOP_LEFT.as_bytes());
    let horizontal = border_glyphs::HORIZONTAL;
    for _ in 0..layout.cols - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(border_glyphs::TOP_RIGHT.as_bytes());
    move_to(&mut buf, 2, 3);
    print_clipped(
        &mut buf,
        &format!(" {} ", boundary_label(snapshot)),
        layout.cols.saturating_sub(6),
    );

    for row in 0..layout.agent_rows {
        let physical = row + 3;
        move_to(&mut buf, physical, 1);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::VERTICAL.as_bytes());
        draw_agent_row(&mut buf, row, layout.agent_cols, screen);
        move_to(&mut buf, physical, layout.cols);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::VERTICAL.as_bytes());
    }
    move_to(&mut buf, layout.rows - 1, 1);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(border_glyphs::BOTTOM_LEFT.as_bytes());
    for _ in 0..layout.cols - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(border_glyphs::BOTTOM_RIGHT.as_bytes());
    if state.panel_open() {
        floating_panel(&mut buf, layout, state, snapshot, started);
    }
    if let Some(request) = &snapshot.audit {
        audit_dialog(&mut buf, layout, request, &snapshot.audit_prompt);
    }
    status_bar::render(&mut buf, layout.cols, layout.rows, state, snapshot, started);
    if state.agent_input_active() && snapshot.audit.is_none() && !screen.hide_cursor() {
        let (cursor_row, cursor_col) = screen.cursor_position();
        move_to(
            &mut buf,
            cursor_row.min(layout.agent_rows - 1) + 3,
            cursor_col.min(layout.agent_cols - 1) + 2,
        );
        buf.extend_from_slice(b"\x1b[?25h");
    }
    buf.extend_from_slice(b"\x1b[0m");
    stdout.write_all(&buf)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_core::audit::{AuditKind, AuditRequest};
    use pvisor_core::operation::PathOperationCounters;

    fn size(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    #[test]
    fn review_panels_show_denied_file_and_network_targets() {
        let mut snapshot = Snapshot::default();
        let mut filesystem = pvisor_core::operation::FilesystemObservation::default();
        filesystem.paths.insert(
            "secrets/key.pem".into(),
            std::collections::BTreeMap::from([(
                "open".into(),
                PathOperationCounters {
                    hits: 1,
                    denied: 1,
                    ..PathOperationCounters::default()
                },
            )]),
        );
        snapshot.filesystem = Some(filesystem);
        snapshot.network = Some(serde_json::json!({
            "policy_denied": 1,
            "targets": {"TCP unexpected.example:443": {"denied": 1}}
        }));
        let files = panel_lines(&snapshot, Panel::Files, Instant::now(), 80).join("\n");
        let network = panel_lines(&snapshot, Panel::Network, Instant::now(), 80).join("\n");
        assert!(files.contains("secrets/key.pem  open: 1 hits, 1 denied"));
        assert!(network.contains("TCP unexpected.example:443: allow 0  deny 1"));

        snapshot.network = Some(serde_json::json!({
            "tcp_flows_denied": 1,
            "tcp_connect_failures": 2
        }));
        assert_eq!(snapshot.network_totals(), (0, 1, 2));
    }

    #[test]
    fn review_overlay_keeps_agent_dimensions_on_wide_and_narrow_terminals() {
        for (cols, rows) in [(30, 8), (80, 24), (156, 90)] {
            let mut state = UiState::default();
            let before = Layout::new(size(cols, rows), &state);
            assert_eq!(state.input(0x1d), None);
            assert_eq!(state.input(b'r'), None);
            let after = Layout::new(size(cols, rows), &state);
            assert_eq!(
                (before.agent_rows, before.agent_cols),
                (after.agent_rows, after.agent_cols)
            );
            assert_eq!(after.agent_rows, rows - 4);
            assert_eq!(after.agent_cols, cols - 2);
            let mut rendered = Vec::new();
            floating_panel(
                &mut rendered,
                after,
                &mut state,
                &Snapshot::default(),
                Instant::now(),
            );
            let mut parser = vt100::Parser::new(rows, cols, 0);
            parser.process(&rendered);
            let (x, y, width, height) = after.floating_rect();
            for (row, col, glyph) in [
                (y - 1, x - 1, "╭"),
                (y - 1, x + width - 2, "╮"),
                (y + height - 2, x - 1, "╰"),
                (y + height - 2, x + width - 2, "╯"),
            ] {
                assert_eq!(parser.screen().cell(row, col).unwrap().contents(), glyph);
            }
            assert!(x >= 2 && x + width <= cols);
            assert!(y >= 3 && y + height < after.rows);
            if cols == 156 {
                assert!(width >= 120, "wide terminal panel should remain readable");
                assert!(height >= 60, "tall terminal panel should show more rows");
            }
        }
    }

    #[test]
    fn audit_prompt_covers_agent_input_and_keeps_decision_keys_visible() {
        let layout = Layout::new(size(80, 24), &UiState::default());
        let snapshot = Snapshot {
            audit: Some(AuditRequest {
                scope: None,
                kind: AuditKind::Network,
                target: "unexpected.example:443".into(),
                reason: "not-in-allowlist".into(),
                host: Some("unexpected.example".into()),
                port: Some(443),
                transport: Some(pvisor_core::NetworkTransport::TcpTunnel),
            }),
            ..Snapshot::default()
        };
        let screen = vt100::Parser::new(layout.agent_rows, layout.agent_cols, 0);
        let mut output = Vec::new();
        render(
            &mut output,
            layout,
            &mut UiState::default(),
            screen.screen(),
            &snapshot,
            Instant::now(),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("NETWORK ACCESS PAUSED"));
        assert!(output.contains("unexpected.example:443"));
        assert!(output.contains("1 This target"));
        assert!(output.contains("2 Host + subdomains"));
        assert!(output.contains("s Session"));
        assert!(output.contains("● Deny"));
        assert!(output.contains("Enter Confirm"));
        assert!(output.contains("w Workspace"));
        assert!(output.contains("u User"));
        assert!(!output.contains("\x1b[?25h"));
        let mut terminal = vt100::Parser::new(24, 80, 0);
        terminal.process(output.as_bytes());
        assert!(terminal.screen().contents().contains("Enter Confirm"));
        for (cols, rows) in [(30, 8), (60, 14), (120, 30)] {
            let layout = Layout::new(size(cols, rows), &UiState::default());
            let mut bytes = Vec::new();
            render(
                &mut bytes,
                layout,
                &mut UiState::default(),
                screen.screen(),
                &snapshot,
                Instant::now(),
            )
            .unwrap();
            let mut terminal = vt100::Parser::new(rows, cols, 0);
            terminal.process(&bytes);
            let content = terminal.screen().contents();
            assert!(content.contains("Deny"), "{cols}x{rows}: {content}");
            if cols == 30 {
                assert!(content.contains("Resize to review"));
            }
        }
    }

    #[test]
    fn shell_input_is_live_until_review_is_open() {
        let mut state = UiState::default();
        for byte in b"codex\r" {
            assert_eq!(state.input(*byte), Some(*byte));
        }
        assert_eq!(state.input(0x1d), None);
        assert_eq!(state.input(b'f'), None);
        assert_eq!(state.panel, Panel::Files);
        assert_eq!(state.input(0x1b), None);
        state.expire_escape(Instant::now() + std::time::Duration::from_millis(200));
        assert_eq!(state.input(b'x'), Some(b'x'));
    }

    #[test]
    fn review_scroll_reaches_last_line_and_clamps_after_resize() {
        let mut state = UiState::default();
        state.input(0x1d);
        state.input(b'l');
        let snapshot = Snapshot {
            log: (0..100).map(|i| format!("entry-{i:03}")).collect(),
            ..Snapshot::default()
        };
        let layout = Layout::new(size(80, 24), &state);
        let mut bytes = Vec::new();
        floating_panel(&mut bytes, layout, &mut state, &snapshot, Instant::now());
        for byte in b"\x1b[F" {
            state.input(*byte);
        }
        bytes.clear();
        floating_panel(&mut bytes, layout, &mut state, &snapshot, Instant::now());
        let mut terminal = vt100::Parser::new(24, 80, 0);
        terminal.process(&bytes);
        let content = terminal.screen().contents();
        assert!(content.contains("entry-099"), "{content}");
        assert!(content.contains("/ 100"));
        assert!(!content.contains("entry-000"));
        let layout = Layout::new(size(120, 60), &state);
        floating_panel(
            &mut Vec::new(),
            layout,
            &mut state,
            &snapshot,
            Instant::now(),
        );
        assert_eq!(state.scroll, state.max_scroll);
        let short = Snapshot {
            log: vec!["only line".into()],
            ..Snapshot::default()
        };
        floating_panel(&mut Vec::new(), layout, &mut state, &short, Instant::now());
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn long_diagnostics_wrap_within_the_log_panel() {
        let wrapped = wrap_log_lines(&["startup message with detail".into()], 10);
        assert_eq!(wrapped, ["startup me", "ssage with", " detail"]);
    }
}

use super::input::{Panel, UiState};
use super::runtime::Snapshot;
use super::{LineStyle, border_glyphs, status_bar};
use anyhow::Result;
use std::io::Write;
use std::time::Instant;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
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
        Panel::Overview => vec![
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
        ],
        Panel::Files => {
            let mut lines = vec![
                format!("{effects} effects   {denied} denied   {failed} failed"),
                String::new(),
            ];
            if let Some(filesystem) = &snapshot.filesystem {
                lines.push(format!(
                    "{} paths   {} overflow",
                    filesystem.paths.len(),
                    filesystem.overflow_hits
                ));
                for (path, operations) in &filesystem.paths {
                    let count: u64 = operations.values().map(|item| item.effects).sum();
                    lines.push(format!("{count:>3}  {path}"));
                }
            }
            lines
        }
        Panel::Network => vec![
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
        ],
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
        Panel::Keys => super::input::help_lines(),
    }
}

fn floating_panel(
    buf: &mut Vec<u8>,
    layout: Layout,
    state: &UiState,
    snapshot: &Snapshot,
    started: Instant,
) {
    let (x, y, width, height) = layout.floating_rect();
    let title = state.panel.title();
    let horizontal = border_glyphs::horizontal(LineStyle::Single);
    move_to(buf, y, x);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::TopLeft,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    for _ in 0..width - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::TopRight,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    move_to(buf, y, x + 2);
    print_clipped(buf, &format!(" pVisor Review · {title} "), width - 4);

    let tabs = if width < 55 {
        "1 Overview  2 Files  3 Net  4 Job  5 Log"
    } else {
        "1 Overview   2 Files   3 Network   4 Job   5 Log"
    };
    let lines = panel_lines(snapshot, state.panel, started, (width - 4) as usize);
    for inner in 0..height - 2 {
        let row = y + inner + 1;
        move_to(buf, row, x);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::vertical(LineStyle::Single).as_bytes());
        buf.extend_from_slice(b"\x1b[48;2;15;19;16m");
        buf.extend_from_slice(" ".repeat((width - 2) as usize).as_bytes());
        move_to(buf, row, x + width - 1);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::vertical(LineStyle::Single).as_bytes());
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
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::BottomLeft,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    for _ in 0..width - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::BottomRight,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    buf.extend_from_slice(b"\x1b[0m");
}

pub(super) fn render(
    stdout: &mut impl Write,
    layout: Layout,
    state: &UiState,
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
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let mut session = format!(
        " pVisor ({}) ",
        workspace.chars().take(16).collect::<String>()
    );
    if UnicodeWidthStr::width(session.as_str()) + 11 > usize::from(layout.cols) {
        session = " pVisor ".into();
    }
    bar_line(&mut buf, 1, layout.cols, &session);
    move_to(
        &mut buf,
        1,
        UnicodeWidthStr::width(session.as_str()) as u16 + 1,
    );
    buf.extend_from_slice(b"\x1b[48;2;167;230;54;38;2;18;22;17;1m");
    print_clipped(
        &mut buf,
        " ❯ Job #1 ",
        layout
            .cols
            .saturating_sub(UnicodeWidthStr::width(session.as_str()) as u16),
    );
    buf.extend_from_slice(b"\x1b[0m");

    move_to(&mut buf, 2, 1);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::TopLeft,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    let horizontal = border_glyphs::horizontal(LineStyle::Single);
    for _ in 0..layout.cols - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::TopRight,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    move_to(&mut buf, 2, 3);
    print_clipped(
        &mut buf,
        &format!(" {agent} "),
        layout.cols.saturating_sub(6),
    );

    for row in 0..layout.agent_rows {
        let physical = row + 3;
        move_to(&mut buf, physical, 1);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::vertical(LineStyle::Single).as_bytes());
        draw_agent_row(&mut buf, row, layout.agent_cols, screen);
        move_to(&mut buf, physical, layout.cols);
        buf.extend_from_slice(ACTIVE.as_bytes());
        buf.extend_from_slice(border_glyphs::vertical(LineStyle::Single).as_bytes());
    }
    move_to(&mut buf, layout.rows - 1, 1);
    buf.extend_from_slice(ACTIVE.as_bytes());
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::BottomLeft,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    for _ in 0..layout.cols - 2 {
        buf.extend_from_slice(horizontal.as_bytes());
    }
    buf.extend_from_slice(
        border_glyphs::corner(
            border_glyphs::Corner::BottomRight,
            LineStyle::Single,
            LineStyle::Single,
            true,
        )
        .as_bytes(),
    );
    if state.panel_open() {
        floating_panel(&mut buf, layout, state, snapshot, started);
    }
    status_bar::render(&mut buf, layout.cols, layout.rows, state, snapshot, started);
    if state.agent_input_active() && !screen.hide_cursor() {
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

    fn size(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
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
            let (x, y, width, height) = after.floating_rect();
            assert!(x >= 2 && x + width <= cols);
            assert!(y >= 3 && y + height < after.rows);
            if cols == 156 {
                assert!(width >= 120, "wide terminal panel should remain readable");
                assert!(height >= 60, "tall terminal panel should show more rows");
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
        assert_eq!(state.input(b'x'), Some(b'x'));
    }

    #[test]
    fn long_diagnostics_wrap_within_the_log_panel() {
        let wrapped = wrap_log_lines(&["startup message with detail".into()], 10);
        assert_eq!(wrapped, ["startup me", "ssage with", " detail"]);
    }
}

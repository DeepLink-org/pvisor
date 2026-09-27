//! One status line that reveals mode-specific shortcuts after the prefix key.

use super::input::{Mode, UiState};
use super::runtime::Snapshot;
use std::io::Write;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

type Rgb = (u8, u8, u8);

const BASE: Rgb = (19, 24, 22);
const RIBBON: Rgb = (24, 29, 27);
const TILE_A: Rgb = (45, 53, 49);
const TILE_B: Rgb = (55, 64, 58);
const LIME: Rgb = (167, 230, 54);
const LIGHT: Rgb = (224, 230, 221);
const AMBER: Rgb = (255, 174, 102);
const RED: Rgb = (255, 111, 111);
const DARK: Rgb = (18, 23, 19);
const ARROW: &str = "";
const LEFT_ARROW: &str = "";

fn move_to(buf: &mut Vec<u8>, row: u16, col: u16) {
    write!(buf, "\x1b[{row};{col}H").unwrap();
}

fn style(buf: &mut Vec<u8>, fg: Rgb, bg: Rgb, bold: bool) {
    write!(
        buf,
        "\x1b[0;{}38;2;{};{};{};48;2;{};{};{}m",
        if bold { "1;" } else { "" },
        fg.0,
        fg.1,
        fg.2,
        bg.0,
        bg.1,
        bg.2
    )
    .unwrap();
}

fn clear_line(buf: &mut Vec<u8>, row: u16, cols: u16, bg: Rgb) {
    move_to(buf, row, 1);
    style(buf, LIGHT, bg, false);
    buf.extend_from_slice(" ".repeat(cols as usize).as_bytes());
    move_to(buf, row, 1);
}

fn elapsed(started: Instant) -> String {
    let seconds = started.elapsed().as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn metrics(snapshot: &Snapshot, elapsed: &str, available: usize) -> String {
    let (hits, effects, denied, failed) = snapshot.file_totals();
    let (allowed, net_denied, net_failed) = snapshot.network_totals();
    let logs = snapshot.log.len();
    let candidates = [
        format!(
            " {elapsed}  │  FILES {hits} hits · {effects} effects · {denied} denied · {failed} failed  │  NET {allowed} allowed · {net_denied} denied · {net_failed} failed  │  LOG {logs}"
        ),
        format!(
            " {elapsed} │ FILES {effects} eff/{denied} deny/{failed} fail │ NET {allowed} ok/{net_denied} deny/{net_failed} fail │ LOG {logs}"
        ),
        format!(
            " {elapsed} │ F {effects}e/{denied}d/{failed}f │ N {allowed}a/{net_denied}d/{net_failed}f │ L {logs}"
        ),
        format!(" {elapsed} F{effects}/{denied} N{allowed}/{net_denied} L{logs}"),
    ];
    candidates
        .into_iter()
        .find(|candidate| UnicodeWidthStr::width(candidate.as_str()) <= available)
        .unwrap_or_else(|| {
            let fallback = format!(" {elapsed}");
            if UnicodeWidthStr::width(fallback.as_str()) <= available {
                fallback
            } else {
                String::new()
            }
        })
}

fn state_color(state: &str) -> Rgb {
    match state {
        "completed" | "running" => LIME,
        "starting" | "pending" | "paused" => AMBER,
        _ => RED,
    }
}

fn render_metrics(buf: &mut Vec<u8>, row: u16, cols: u16, snapshot: &Snapshot, started: Instant) {
    clear_line(buf, row, cols, BASE);
    let state = if snapshot.audit.is_some() {
        "paused"
    } else {
        snapshot
            .record
            .as_ref()
            .map_or("starting", |run| run.state.as_str())
    };
    let chip = format!(" {} ", state.to_ascii_uppercase());
    let chip_width = UnicodeWidthStr::width(chip.as_str());
    let color = state_color(state);
    style(buf, DARK, color, true);
    buf.extend_from_slice(chip.as_bytes());
    style(buf, color, BASE, false);
    buf.extend_from_slice(ARROW.as_bytes());
    let hint = " Ctrl-]  MENU ";
    let hint_width = UnicodeWidthStr::width(hint) + 1;
    let remaining = usize::from(cols).saturating_sub(chip_width + 1 + hint_width + 1);
    let summary = metrics(snapshot, &elapsed(started), remaining);
    style(buf, LIGHT, BASE, false);
    buf.extend_from_slice(summary.as_bytes());
    move_to(buf, row, cols - hint_width as u16 + 1);
    style(buf, TILE_A, BASE, false);
    buf.extend_from_slice(LEFT_ARROW.as_bytes());
    style(buf, DARK, TILE_A, true);
    buf.extend_from_slice(hint.as_bytes());
    buf.extend_from_slice(b"\x1b[0m");
}

fn short_label(label: &str, compact: bool) -> &str {
    if !compact {
        return label;
    }
    match label {
        "Review" => "Rev",
        "Network" => "Net",
        "Permissions" => "Perm",
        "Cancel" => "Back",
        "Select" => "Views",
        _ => label,
    }
}

fn render_shortcuts(buf: &mut Vec<u8>, row: u16, cols: u16, state: &UiState) -> usize {
    clear_line(buf, row, cols, RIBBON);
    let mode = if cols < 60 || state.mode != Mode::Panel {
        state.mode.label().to_string()
    } else {
        format!("REVIEW:{}", state.panel.title().to_ascii_uppercase())
    };
    let chip = format!(" {mode} ");
    let mut used = UnicodeWidthStr::width(chip.as_str());
    style(buf, DARK, LIME, true);
    buf.extend_from_slice(chip.as_bytes());
    let mut previous_bg = LIME;
    let mut truncated = false;
    for (index, hint) in state.ribbon_hints().into_iter().enumerate() {
        let Some((key, label)) = hint.split_once(' ') else {
            continue;
        };
        let key = if cols < 105 && key == "Ctrl-]" {
            "^]"
        } else {
            key
        };
        let label = short_label(label, cols < 105);
        let compact = cols < 105;
        let body = if compact {
            format!("{key} {label}")
        } else {
            format!(" <{key}> {label} ")
        };
        let width = 1 + UnicodeWidthStr::width(body.as_str());
        if used + width > usize::from(cols) {
            if used + 2 <= usize::from(cols) {
                style(buf, previous_bg, RIBBON, false);
                buf.extend_from_slice(ARROW.as_bytes());
                style(buf, AMBER, RIBBON, true);
                buf.extend_from_slice("…".as_bytes());
                used += 2;
            }
            truncated = true;
            break;
        }
        let bg = if index % 2 == 0 { TILE_A } else { TILE_B };
        style(buf, previous_bg, bg, false);
        buf.extend_from_slice(ARROW.as_bytes());
        style(buf, AMBER, bg, true);
        if compact {
            write!(buf, "{key}").unwrap();
        } else {
            write!(buf, " <{key}>").unwrap();
        }
        style(buf, LIGHT, bg, false);
        if compact {
            write!(buf, " {label}").unwrap();
        } else {
            write!(buf, " {label} ").unwrap();
        }
        used += width;
        previous_bg = bg;
    }
    if !truncated && used < usize::from(cols) {
        style(buf, previous_bg, RIBBON, false);
        buf.extend_from_slice(ARROW.as_bytes());
        used += 1;
    }
    buf.extend_from_slice(b"\x1b[0m");
    used
}

pub(super) fn render(
    buf: &mut Vec<u8>,
    cols: u16,
    row: u16,
    state: &UiState,
    snapshot: &Snapshot,
    started: Instant,
) {
    if state.mode == Mode::Agent {
        render_metrics(buf, row, cols, snapshot, started);
    } else {
        render_shortcuts(buf, row, cols, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_choose_readable_detail_for_terminal_width() {
        let snapshot = Snapshot::default();
        let wide = metrics(&snapshot, "01:23", 140);
        assert!(wide.contains("hits"));
        assert!(wide.contains("failed"));
        let medium = metrics(&snapshot, "01:23", 70);
        assert!(medium.contains("FILES"));
        assert!(medium.contains("NET"));
        assert!(medium.contains("LOG"));
        let narrow = metrics(&snapshot, "01:23", 27);
        assert!(UnicodeWidthStr::width(narrow.as_str()) <= 27);
    }

    #[test]
    fn shortcut_ribbon_never_exceeds_terminal_width() {
        for cols in [30, 45, 80, 120] {
            for mode in [Mode::Command, Mode::Panel] {
                let state = UiState {
                    mode,
                    ..UiState::default()
                };
                let used = render_shortcuts(&mut Vec::new(), 24, cols, &state);
                assert!(used <= usize::from(cols), "{mode:?} at {cols} columns");
            }
        }
    }

    #[test]
    fn normal_bar_only_hints_at_prefix_and_command_bar_reveals_actions() {
        let snapshot = Snapshot::default();
        let mut state = UiState::default();
        let mut normal = Vec::new();
        render(&mut normal, 80, 24, &state, &snapshot, Instant::now());
        let normal = String::from_utf8(normal).unwrap();
        assert!(normal.contains("Ctrl-]"));
        assert!(!normal.contains("Review"));

        state.input(0x1d);
        let mut command = Vec::new();
        let used = render_shortcuts(&mut command, 24, 80, &state);
        let command = String::from_utf8(command).unwrap();
        assert!(used <= 80);
        for label in ["Rev", "Files", "Net", "Job", "Log", "Keys", "Send"] {
            assert!(command.contains(label), "missing {label} at 80 columns");
        }
    }
}

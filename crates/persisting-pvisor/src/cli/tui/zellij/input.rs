//! Mode-scoped key bindings following Zellij's input-mode/action pattern.
//! pVisor binds only keys it implements; Agent mode forwards everything else.

use std::time::{Duration, Instant};

const PREFIX: u8 = 0x1d; // Ctrl-]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Agent,
    Command,
    Panel,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "NORMAL",
            Self::Command => "COMMAND",
            Self::Panel => "REVIEW",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    Overview,
    Files,
    Network,
    Run,
    Log,
    Permissions,
    Keys,
}

impl Panel {
    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Files => "Files",
            Self::Network => "Network",
            Self::Run => "Job",
            Self::Log => "Log",
            Self::Permissions => "Permissions",
            Self::Keys => "Keys",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Overview => Self::Files,
            Self::Files => Self::Network,
            Self::Network => Self::Run,
            Self::Run => Self::Log,
            Self::Log => Self::Permissions,
            Self::Permissions | Self::Keys => Self::Overview,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    SwitchTo(Mode),
    Open(Panel),
    NextPanel,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    First,
    Last,
    SendPrefix,
}

#[derive(Clone, Copy, Debug)]
struct Binding {
    mode: Mode,
    key: u8,
    action: Action,
    hint: &'static str,
}

macro_rules! bind {
    ($mode:ident, $key:expr, $action:expr, $hint:literal) => {
        Binding {
            mode: Mode::$mode,
            key: $key,
            action: $action,
            hint: $hint,
        }
    };
}

// Like Zellij's per-InputMode keymaps, the same byte can perform different
// actions in different modes. Empty hints are aliases hidden from the bar.
const BINDINGS: &[Binding] = &[
    bind!(
        Agent,
        PREFIX,
        Action::SwitchTo(Mode::Command),
        "Ctrl-] Menu"
    ),
    bind!(Command, b'r', Action::Open(Panel::Overview), "r Review"),
    bind!(Command, b'f', Action::Open(Panel::Files), "f Files"),
    bind!(Command, b'n', Action::Open(Panel::Network), "n Network"),
    bind!(Command, b'u', Action::Open(Panel::Run), "u Job"),
    bind!(Command, b'l', Action::Open(Panel::Log), "l Log"),
    bind!(
        Command,
        b'p',
        Action::Open(Panel::Permissions),
        "p Permissions"
    ),
    bind!(Command, b'?', Action::Open(Panel::Keys), "? Keys"),
    bind!(Command, 0x1b, Action::SwitchTo(Mode::Agent), "Esc Cancel"),
    bind!(Command, b'q', Action::SwitchTo(Mode::Agent), ""),
    bind!(Command, PREFIX, Action::SendPrefix, "Ctrl-] Send"),
    bind!(Command, b'1', Action::Open(Panel::Overview), ""),
    bind!(Command, b'2', Action::Open(Panel::Files), ""),
    bind!(Command, b'3', Action::Open(Panel::Network), ""),
    bind!(Command, b'4', Action::Open(Panel::Run), ""),
    bind!(Command, b'5', Action::Open(Panel::Log), ""),
    bind!(Command, b'6', Action::Open(Panel::Permissions), ""),
    bind!(Panel, b'\t', Action::NextPanel, "Tab View"),
    bind!(Panel, b'j', Action::ScrollDown, "↑/↓/Wheel Scroll"),
    bind!(Panel, 0x02, Action::PageUp, "PgUp/PgDn Page"),
    bind!(Panel, 0x06, Action::PageDown, ""),
    bind!(Panel, b'g', Action::First, "Home/End Jump"),
    bind!(Panel, b'G', Action::Last, ""),
    bind!(Panel, b'k', Action::ScrollUp, ""),
    bind!(Panel, b'1', Action::Open(Panel::Overview), "1-6 Select"),
    bind!(Panel, b'2', Action::Open(Panel::Files), ""),
    bind!(Panel, b'3', Action::Open(Panel::Network), ""),
    bind!(Panel, b'4', Action::Open(Panel::Run), ""),
    bind!(Panel, b'5', Action::Open(Panel::Log), ""),
    bind!(Panel, b'6', Action::Open(Panel::Permissions), ""),
    bind!(Panel, b'?', Action::Open(Panel::Keys), "? Keys"),
    bind!(Panel, 0x1b, Action::SwitchTo(Mode::Agent), "Esc Close"),
    bind!(Panel, PREFIX, Action::SwitchTo(Mode::Agent), ""),
    bind!(Panel, b'q', Action::SwitchTo(Mode::Agent), ""),
    bind!(Panel, b'm', Action::SwitchTo(Mode::Agent), ""),
    bind!(Panel, b'r', Action::Open(Panel::Overview), ""),
    bind!(Panel, b'f', Action::Open(Panel::Files), ""),
    bind!(Panel, b'n', Action::Open(Panel::Network), ""),
    bind!(Panel, b'u', Action::Open(Panel::Run), ""),
    bind!(Panel, b'l', Action::Open(Panel::Log), ""),
    bind!(Panel, b'p', Action::Open(Panel::Permissions), ""),
];

pub(super) fn help_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for (mode, heading) in [
        (Mode::Agent, "AGENT INPUT"),
        (Mode::Command, "COMMAND MODE"),
        (Mode::Panel, "REVIEW PANEL"),
    ] {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(heading.into());
        lines.extend(
            BINDINGS
                .iter()
                .filter(|binding| binding.mode == mode && !binding.hint.is_empty())
                .map(|binding| format!("  {}", binding.hint)),
        );
    }
    lines.push(String::new());
    lines.push("Unbound keys in Agent mode go to the shell.".into());
    lines
}

#[derive(Debug)]
pub(super) struct UiState {
    pub mode: Mode,
    pub panel: Panel,
    pub scroll: usize,
    pub page_rows: usize,
    pub max_scroll: usize,
    pub escape: Vec<u8>,
    pub escape_started: Option<Instant>,
    pub permission: usize,
    pub forget_pending: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            mode: Mode::Agent,
            panel: Panel::Overview,
            scroll: 0,
            page_rows: 1,
            max_scroll: usize::MAX,
            escape: Vec::new(),
            escape_started: None,
            permission: 0,
            forget_pending: false,
        }
    }
}

impl UiState {
    pub fn panel_open(&self) -> bool {
        self.mode == Mode::Panel
    }

    pub fn agent_input_active(&self) -> bool {
        self.mode == Mode::Agent
    }

    /// Shortcut tiles are selected from the same bindings that handle input.
    pub fn ribbon_hints(&self) -> Vec<&'static str> {
        match self.mode {
            Mode::Agent => BINDINGS
                .iter()
                .filter(|binding| binding.mode == Mode::Agent && !binding.hint.is_empty())
                .map(|binding| binding.hint)
                .collect(),
            Mode::Command | Mode::Panel => {
                let mut hints: Vec<_> = BINDINGS
                    .iter()
                    .filter(|binding| {
                        !binding.hint.is_empty()
                            && binding.mode == self.mode
                            && matches!(binding.action, Action::SwitchTo(Mode::Agent))
                    })
                    .map(|binding| binding.hint)
                    .collect();
                hints.extend(
                    BINDINGS
                        .iter()
                        .filter(|binding| {
                            !binding.hint.is_empty()
                                && binding.mode == self.mode
                                && !matches!(binding.action, Action::SwitchTo(Mode::Agent))
                        })
                        .map(|binding| binding.hint),
                );
                hints
            }
        }
    }

    // Escape sequences can arrive in separate stdin reads. A lone Esc closes
    // Review only after a short timeout; arrow/mouse bytes never reach the PTY.
    pub fn expire_escape(&mut self, now: Instant) -> bool {
        if self
            .escape_started
            .is_some_and(|at| now.duration_since(at) >= Duration::from_millis(150))
        {
            if self.escape == [0x1b] {
                self.mode = Mode::Agent;
            }
            self.escape.clear();
            self.escape_started = None;
            return true;
        }
        false
    }

    fn navigation(&mut self, action: Action) {
        let offset = if self.panel == Panel::Permissions {
            &mut self.permission
        } else {
            &mut self.scroll
        };
        *offset = match action {
            Action::ScrollUp => offset.saturating_sub(1),
            Action::ScrollDown => offset.saturating_add(1),
            Action::PageUp => offset.saturating_sub(self.page_rows),
            Action::PageDown => offset.saturating_add(self.page_rows),
            Action::First => 0,
            Action::Last => self.max_scroll,
            _ => return,
        };
        if self.panel != Panel::Permissions {
            *offset = (*offset).min(self.max_scroll);
        }
    }

    /// Returns a byte for the Agent PTY, or consumes it as a UI binding.
    pub fn input(&mut self, byte: u8) -> Option<u8> {
        self.forget_pending = false;
        if self.panel_open() && (byte == 0x1b || !self.escape.is_empty()) {
            if self.escape.is_empty() {
                self.escape_started = Some(Instant::now());
            }
            self.escape.push(byte);
            if self.escape.len() == 1 || (self.escape.len() == 2 && matches!(byte, b'[' | b'O')) {
                return None;
            }
            if self.escape.len() > 32 || (self.escape.len() == 2 && !matches!(byte, b'[' | b'O')) {
                self.escape.clear();
                self.escape_started = None;
                return None;
            }
            if (0x40..=0x7e).contains(&byte) {
                let action = match self.escape.as_slice() {
                    b"\x1b[A" | b"\x1bOA" => Some(Action::ScrollUp),
                    b"\x1b[B" | b"\x1bOB" => Some(Action::ScrollDown),
                    b"\x1b[5~" => Some(Action::PageUp),
                    b"\x1b[6~" => Some(Action::PageDown),
                    b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => Some(Action::First),
                    b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Some(Action::Last),
                    mouse if mouse.starts_with(b"\x1b[<64;") && byte == b'M' => {
                        Some(Action::ScrollUp)
                    }
                    mouse if mouse.starts_with(b"\x1b[<65;") && byte == b'M' => {
                        Some(Action::ScrollDown)
                    }
                    _ => None,
                };
                self.escape.clear();
                self.escape_started = None;
                if let Some(action) = action {
                    self.navigation(action);
                }
            }
            return None;
        }
        let Some(binding) = BINDINGS
            .iter()
            .find(|binding| binding.mode == self.mode && binding.key == byte)
        else {
            return (self.mode == Mode::Agent).then_some(byte);
        };
        match binding.action {
            Action::SwitchTo(mode) => self.mode = mode,
            Action::Open(panel) => {
                self.mode = Mode::Panel;
                self.panel = panel;
                self.scroll = 0;
            }
            Action::NextPanel => {
                self.panel = self.panel.next();
                self.scroll = 0;
            }
            action @ (Action::ScrollUp
            | Action::ScrollDown
            | Action::PageUp
            | Action::PageDown
            | Action::First
            | Action::Last) => self.navigation(action),
            Action::SendPrefix => {
                self.mode = Mode::Agent;
                return Some(PREFIX);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_navigation_consumes_sequences_and_obeys_viewport_bounds() {
        let mut state = UiState::default();
        state.input(PREFIX);
        state.input(b'l');
        state.page_rows = 10;
        state.max_scroll = 23;
        for (sequence, expected) in [
            (b"\x1b[B".as_slice(), 1),
            (b"\x1b[6~".as_slice(), 11),
            (b"\x1b[F".as_slice(), 23),
            (b"j".as_slice(), 23),
            (b"\x1b[5~".as_slice(), 13),
            (b"\x1b[<64;30;12M".as_slice(), 12),
            (b"\x1b[<65;30;12M".as_slice(), 13),
            (b"\x1b[H".as_slice(), 0),
            (b"\x1b[A".as_slice(), 0),
            (b"\x1b[99~".as_slice(), 0),
        ] {
            for byte in sequence {
                assert_eq!(state.input(*byte), None);
            }
            assert_eq!(state.scroll, expected);
            assert_eq!(state.mode, Mode::Panel);
        }
        state.input(0x1b);
        assert!(!state.expire_escape(Instant::now()));
        assert!(state.expire_escape(Instant::now() + Duration::from_millis(200)));
        for byte in b"\x1b[B" {
            assert_eq!(state.input(*byte), Some(*byte));
        }
    }

    #[test]
    fn bindings_are_unique_within_each_mode() {
        for (index, binding) in BINDINGS.iter().enumerate() {
            assert!(
                !BINDINGS[..index]
                    .iter()
                    .any(|previous| previous.mode == binding.mode && previous.key == binding.key)
            );
        }
    }

    #[test]
    fn agent_keystrokes_pass_through_and_prefix_opens_control_mode() {
        let mut state = UiState::default();
        for byte in b"codex\r" {
            assert_eq!(state.input(*byte), Some(*byte));
        }
        assert_eq!(state.input(PREFIX), None);
        assert_eq!(state.mode, Mode::Command);
        assert_eq!(state.input(b'f'), None);
        assert_eq!(state.mode, Mode::Panel);
        assert_eq!(state.panel, Panel::Files);
        assert_eq!(state.input(b'j'), None);
        assert_eq!(state.scroll, 1);
        assert_eq!(state.input(0x1b), None);
        state.expire_escape(Instant::now() + Duration::from_millis(200));
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.input(b'f'), Some(b'f'));
    }

    #[test]
    fn doubling_prefix_sends_literal_control_byte_to_agent() {
        let mut state = UiState::default();
        assert_eq!(state.input(PREFIX), None);
        assert_eq!(state.input(PREFIX), Some(PREFIX));
        assert_eq!(state.mode, Mode::Agent);
    }

    #[test]
    fn displayed_hints_come_from_active_mode_bindings() {
        let mut state = UiState::default();
        assert_eq!(state.ribbon_hints().join("  "), "Ctrl-] Menu");
        state.input(PREFIX);
        assert!(state.ribbon_hints().join("  ").contains("r Review"));
        assert!(!state.ribbon_hints().join("  ").contains("Tab View"));
        state.input(b'r');
        assert!(state.ribbon_hints().join("  ").contains("Tab View"));
    }

    #[test]
    fn log_panel_is_reachable_and_shell_input_resumes_after_close() {
        let mut state = UiState::default();
        assert_eq!(state.input(PREFIX), None);
        assert_eq!(state.input(b'l'), None);
        assert_eq!(state.panel, Panel::Log);
        assert_eq!(state.input(b'1'), None);
        assert_eq!(state.panel, Panel::Overview);
        assert_eq!(state.input(b'5'), None);
        assert_eq!(state.panel, Panel::Log);
        assert_eq!(state.input(0x1b), None);
        state.expire_escape(Instant::now() + Duration::from_millis(200));
        assert_eq!(state.input(b'l'), Some(b'l'));
    }
}

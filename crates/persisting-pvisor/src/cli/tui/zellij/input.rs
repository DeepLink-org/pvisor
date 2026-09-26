//! Mode-scoped key bindings following Zellij's input-mode/action pattern.
//! pVisor binds only keys it implements; Agent mode forwards everything else.

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
            Self::Keys => "Keys",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Overview => Self::Files,
            Self::Files => Self::Network,
            Self::Network => Self::Run,
            Self::Run => Self::Log,
            Self::Log | Self::Keys => Self::Overview,
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
    bind!(Command, b'?', Action::Open(Panel::Keys), "? Keys"),
    bind!(Command, 0x1b, Action::SwitchTo(Mode::Agent), "Esc Cancel"),
    bind!(Command, b'q', Action::SwitchTo(Mode::Agent), ""),
    bind!(Command, PREFIX, Action::SendPrefix, "Ctrl-] Send"),
    bind!(Command, b'1', Action::Open(Panel::Overview), ""),
    bind!(Command, b'2', Action::Open(Panel::Files), ""),
    bind!(Command, b'3', Action::Open(Panel::Network), ""),
    bind!(Command, b'4', Action::Open(Panel::Run), ""),
    bind!(Command, b'5', Action::Open(Panel::Log), ""),
    bind!(Panel, b'\t', Action::NextPanel, "Tab View"),
    bind!(Panel, b'j', Action::ScrollDown, "j/k Scroll"),
    bind!(Panel, b'k', Action::ScrollUp, ""),
    bind!(Panel, b'1', Action::Open(Panel::Overview), "1-5 Select"),
    bind!(Panel, b'2', Action::Open(Panel::Files), ""),
    bind!(Panel, b'3', Action::Open(Panel::Network), ""),
    bind!(Panel, b'4', Action::Open(Panel::Run), ""),
    bind!(Panel, b'5', Action::Open(Panel::Log), ""),
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
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            mode: Mode::Agent,
            panel: Panel::Overview,
            scroll: 0,
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

    pub fn hints(&self) -> String {
        BINDINGS
            .iter()
            .filter(|binding| binding.mode == self.mode && !binding.hint.is_empty())
            .map(|binding| binding.hint)
            .collect::<Vec<_>>()
            .join("  ")
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

    /// Returns a byte for the Agent PTY, or consumes it as a UI binding.
    pub fn input(&mut self, byte: u8) -> Option<u8> {
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
            Action::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
            Action::ScrollDown => self.scroll = self.scroll.saturating_add(1),
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
        assert_eq!(state.hints(), "Ctrl-] Menu");
        state.input(PREFIX);
        assert!(state.hints().contains("r Review"));
        assert!(!state.hints().contains("Tab View"));
        state.input(b'r');
        assert!(state.hints().contains("Tab View"));
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
        assert_eq!(state.input(b'l'), Some(b'l'));
    }
}

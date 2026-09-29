//! CLI entry point for the Zellij-inspired native pVisor terminal UI.

mod zellij;

pub(super) use zellij::{announce_stage, available, init_child_context, is_child, run};

pub(crate) use zellij::diagnostic;

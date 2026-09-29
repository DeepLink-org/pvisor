//! Zellij-inspired TUI. The pane border glyphs are adapted from Zellij;
//! pVisor's PTY runtime, keymap, and Run panels live alongside them.
//! Original source: https://github.com/zellij-org/zellij/tree/fc400dfef9ee79ca1412831f73d1f3c79699ea3f/zellij-server/src/ui
//! Copyright (c) 2020 Zellij contributors. MIT license: see LICENSE.md.

mod audit_ui;
mod input;
mod runtime;
mod status_bar;
mod view;

pub(super) mod border_glyphs;
pub(crate) use runtime::{announce_stage, available, init_child_context, is_child, run};

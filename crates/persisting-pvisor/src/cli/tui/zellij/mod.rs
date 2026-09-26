//! Zellij-inspired TUI. The pane border glyphs are adapted from Zellij;
//! pVisor's PTY runtime, keymap, and Run panels live alongside them.
//! Original source: https://github.com/zellij-org/zellij/tree/fc400dfef9ee79ca1412831f73d1f3c79699ea3f/zellij-server/src/ui
//! Copyright (c) 2020 Zellij contributors. MIT license: see LICENSE.md.

#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineStyle {
    Single,
    Double,
    Heavy,
    Dashed,
    HeavyDashed,
}

pub(super) mod boundary_type {
    pub const TOP_RIGHT: &str = "┐";
    pub const TOP_RIGHT_ROUND: &str = "╮";
    pub const VERTICAL: &str = "│";
    pub const HORIZONTAL: &str = "─";
    pub const TOP_LEFT: &str = "┌";
    pub const TOP_LEFT_ROUND: &str = "╭";
    pub const BOTTOM_RIGHT: &str = "┘";
    pub const BOTTOM_RIGHT_ROUND: &str = "╯";
    pub const BOTTOM_LEFT: &str = "└";
    pub const BOTTOM_LEFT_ROUND: &str = "╰";
    pub const VERTICAL_LEFT: &str = "┤";
    pub const VERTICAL_RIGHT: &str = "├";
    pub const HORIZONTAL_DOWN: &str = "┬";
    pub const HORIZONTAL_UP: &str = "┴";
    pub const CROSS: &str = "┼";
}

mod input;
mod runtime;
mod status_bar;
mod view;

pub(super) mod border_glyphs;
pub(crate) use runtime::{
    announce_stage, available, diagnostic, init_child_context, is_child, run,
};

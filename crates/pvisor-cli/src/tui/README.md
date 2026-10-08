# Zellij attribution

This directory contains pVisor's complete native TUI implementation: the PTY
runtime, terminal renderer, review panels, and mode-scoped keymap. The keymap
design follows Zellij's [input modes and keybindings](https://zellij.dev/documentation/keybindings-modes),
and the bottom ribbon takes visual cues from Zellij's mode-sensitive status bar.
These files are original pVisor code except for the adapted border glyphs below.

`border_glyphs.rs` is adapted from Zellij's
[`zellij-server/src/ui/border_glyphs.rs`](https://github.com/zellij-org/zellij/blob/fc400dfef9ee79ca1412831f73d1f3c79699ea3f/zellij-server/src/ui/border_glyphs.rs)
at commit `fc400dfef9ee79ca1412831f73d1f3c79699ea3f`.

Only the rounded single-line glyphs used by pVisor are retained.
The original copyright and MIT permission notice are in [LICENSE.md](LICENSE.md).

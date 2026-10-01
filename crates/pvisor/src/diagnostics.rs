//! Host diagnostics shared by runtime services and the CLI.
//! The frontend selects the destination; services do not depend on the TUI.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn init(path: Option<PathBuf>) {
    let _ = LOG_CONTEXT.set(path);
}

pub(crate) fn diagnostic(args: std::fmt::Arguments<'_>) {
    if let Some(Some(path)) = LOG_CONTEXT.get()
        && let Ok(mut file) = OpenOptions::new().append(true).open(path)
        && writeln!(file, "{args}").is_ok()
    {
        return;
    }
    eprintln!("{args}");
}

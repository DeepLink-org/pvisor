//! Host diagnostics shared by runtime services and the CLI.
//! The frontend selects the destination; services do not depend on the TUI.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub(crate) const INHERITED_LOG_ENV: &str = "PVISOR_DIAGNOSTICS_FD";
pub(crate) const INHERITED_LOG_FD: std::os::fd::RawFd = 201;
static INHERITED_LOG: OnceLock<Mutex<File>> = OnceLock::new();

/// Only the VM runner receives this pre-opened host diagnostic descriptor.
/// Keep it out of guest exec, and avoid opening frontend paths inside confinement.
pub fn init_inherited() {
    use std::os::fd::FromRawFd;
    if INHERITED_LOG.get().is_some() || std::env::var(INHERITED_LOG_ENV).as_deref() != Ok("201") {
        return;
    }
    if unsafe { libc::fcntl(INHERITED_LOG_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == 0 {
        let file = unsafe { File::from_raw_fd(INHERITED_LOG_FD) };
        let _ = INHERITED_LOG.set(Mutex::new(file));
    }
    unsafe { std::env::remove_var(INHERITED_LOG_ENV) };
}

pub(crate) fn runner_output() -> Option<File> {
    let path = LOG_CONTEXT.get()?.as_ref()?;
    OpenOptions::new().append(true).open(path).ok()
}

static LOG_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn init(path: Option<PathBuf>) {
    let _ = LOG_CONTEXT.set(path);
}

pub fn diagnostic(args: std::fmt::Arguments<'_>) {
    // Parent and runner append to the same file. Format first so one record
    // is submitted in one write rather than interleaved formatting fragments.
    let mut line = args.to_string();
    line.push('\n');
    if let Some(output) = INHERITED_LOG.get()
        && let Ok(mut output) = output.lock()
        && output.write_all(line.as_bytes()).is_ok()
    {
        return;
    }
    if let Some(Some(path)) = LOG_CONTEXT.get()
        && let Ok(mut file) = OpenOptions::new().append(true).open(path)
        && file.write_all(line.as_bytes()).is_ok()
    {
        return;
    }
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

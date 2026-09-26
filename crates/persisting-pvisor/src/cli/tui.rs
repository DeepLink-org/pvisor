//! Interactive PTY shell around a Run. The Agent keeps its own terminal UI;
//! pVisor reserves one row for status and temporarily shrinks the PTY for tabs.

use crate::runtime::{RunRecord, control_observations};
use anyhow::{Context, Result};
use persisting_control::ir::run::FilesystemObservation;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CHILD_MARKER: &str = "PVISOR_UI_CHILD";
const STAGE_FILE: &str = "PVISOR_UI_STAGE_FILE";
const HOTKEY: u8 = 0x1d; // Ctrl-]
static CHILD_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(super) fn init_child_context() {
    let path = if std::env::var_os(CHILD_MARKER).is_some() {
        std::env::var_os(STAGE_FILE).map(PathBuf::from)
    } else {
        None
    };
    // This runs before the Tokio runtime and any Agent environment is built.
    unsafe {
        std::env::remove_var(CHILD_MARKER);
        std::env::remove_var(STAGE_FILE);
    }
    let _ = CHILD_CONTEXT.set(path);
}

pub(super) fn announce_stage(stage: &Path) {
    if let Some(Some(path)) = CHILD_CONTEXT.get() {
        let _ = std::fs::write(path, stage.as_os_str().as_encoded_bytes());
    }
}

pub(super) fn available() -> bool {
    CHILD_CONTEXT.get().is_some_and(Option::is_none)
        && unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1
        }
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

pub(super) fn is_child() -> bool {
    CHILD_CONTEXT.get().is_some_and(Option::is_some)
}

struct TerminalGuard {
    original: libc::termios,
}

impl TerminalGuard {
    fn raw() -> Result<Self> {
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = std::io::stdout().write_all(b"\x1b[0m\x1b[?25h\r\n");
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
    }
}

struct ChildCleanup(std::process::Child);

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[derive(Default)]
struct Snapshot {
    stage: Option<PathBuf>,
    record: Option<RunRecord>,
    filesystem: Option<FilesystemObservation>,
    network: Option<serde_json::Value>,
}

impl Snapshot {
    fn refresh(&mut self, stage_file: &Path) {
        if self.stage.is_none() {
            self.stage = std::fs::read(stage_file)
                .ok()
                .filter(|bytes| !bytes.is_empty())
                .map(|bytes| {
                    use std::os::unix::ffi::OsStringExt;
                    PathBuf::from(OsString::from_vec(bytes))
                });
        }
        let Some(stage) = &self.stage else { return };
        self.record = RunRecord::read(stage).ok();
        if let Ok(value) = control_observations(stage) {
            self.filesystem = value
                .get("filesystem")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            self.network = value
                .get("network")
                .filter(|value| !value.is_null())
                .cloned();
        } else if let Some(record) = &self.record {
            self.filesystem = record.filesystem_observation.clone();
            self.network = record
                .network_interception_metrics
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok());
        }
    }

    fn file_totals(&self) -> (u64, u64, u64, u64) {
        let Some(fs) = &self.filesystem else {
            return (0, 0, 0, 0);
        };
        fs.paths
            .values()
            .flat_map(|ops| ops.values())
            .fold((0, 0, 0, 0), |sum, item| {
                (
                    sum.0 + item.hits,
                    sum.1 + item.effects,
                    sum.2 + item.denied,
                    sum.3 + item.failed,
                )
            })
    }

    fn network_totals(&self) -> (u64, u64, u64) {
        let number = |key: &str| {
            self.network
                .as_ref()
                .and_then(|value| value.get(key))
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        };
        (
            number("policy_allowed"),
            number("policy_denied"),
            number("failures"),
        )
    }
}

fn terminal_size() -> libc::winsize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } != 0 {
        size.ws_row = 24;
        size.ws_col = 80;
    }
    size.ws_row = size.ws_row.max(8);
    size.ws_col = size.ws_col.max(30);
    size
}

fn child_rows(size: libc::winsize, details: bool) -> u16 {
    if details {
        (size.ws_row / 2).max(4)
    } else {
        size.ws_row - 1
    }
}

fn resize_pty(master: &File, size: libc::winsize, details: bool) {
    let mut child_size = size;
    child_size.ws_row = child_rows(size, details);
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &child_size) };
}

fn row(stdout: &mut impl Write, line: u16, cols: u16, text: &str, inverse: bool) -> Result<()> {
    write!(stdout, "\x1b[{line};1H\x1b[2K")?;
    if inverse {
        write!(stdout, "\x1b[7m")?;
    }
    let sanitized = text.replace('\x1b', " ").replace(['\r', '\n'], " ");
    for ch in sanitized.chars().take(cols as usize) {
        write!(stdout, "{ch}")?;
    }
    if inverse {
        write!(stdout, "\x1b[0m")?;
    }
    Ok(())
}

fn clear_panel(stdout: &mut impl Write, size: libc::winsize) -> Result<()> {
    write!(stdout, "\x1b7")?;
    for line in child_rows(size, true) + 1..=size.ws_row {
        write!(stdout, "\x1b[{line};1H\x1b[2K")?;
    }
    write!(stdout, "\x1b8")?;
    stdout.flush()?;
    Ok(())
}

fn draw(
    stdout: &mut impl Write,
    size: libc::winsize,
    details: bool,
    tab: usize,
    scroll: usize,
    snapshot: &Snapshot,
    started: Instant,
) -> Result<()> {
    write!(stdout, "\x1b7")?;
    let (hits, effects, denied, failed) = snapshot.file_totals();
    let (net_allowed, net_denied, net_failed) = snapshot.network_totals();
    let state = snapshot
        .record
        .as_ref()
        .map_or("starting", |record| record.state.as_str());
    if !details {
        let bar = format!(
            " pVisor {state}  {:>4}s | file {hits} hits / {effects} effects / {denied} denied / {failed} failed | net {net_allowed} ok / {net_denied} denied / {net_failed} failed | Ctrl-] details ",
            started.elapsed().as_secs()
        );
        row(stdout, size.ws_row, size.ws_col, &bar, true)?;
    } else {
        let top = child_rows(size, true) + 1;
        let tabs = ["Overview", "Files", "Network", "Run"];
        row(
            stdout,
            top,
            size.ws_col,
            &format!(
                " pVisor  {}  | Tab/1-4 switch  j/k scroll  Esc/Ctrl-] close",
                tabs.iter()
                    .enumerate()
                    .map(|(index, name)| if index == tab {
                        format!("[{name}]")
                    } else {
                        name.to_string()
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            ),
            true,
        )?;
        let lines = match tab {
            0 => vec![
                format!(
                    "State: {state}    Elapsed: {}s",
                    started.elapsed().as_secs()
                ),
                format!(
                    "Filesystem: {hits} hits, {effects} effects, {denied} denied, {failed} failed"
                ),
                format!("Network: {net_allowed} allowed, {net_denied} denied, {net_failed} failed"),
                format!(
                    "Stage: {}",
                    snapshot
                        .stage
                        .as_ref()
                        .map_or("pending".into(), |path| path.display().to_string())
                ),
            ],
            1 => {
                let mut lines = vec![format!(
                    "Paths: {}  overflow: {}",
                    snapshot.filesystem.as_ref().map_or(0, |fs| fs.paths.len()),
                    snapshot
                        .filesystem
                        .as_ref()
                        .map_or(0, |fs| fs.overflow_hits)
                )];
                if let Some(fs) = &snapshot.filesystem {
                    for (path, operations) in &fs.paths {
                        let (hits, effects, denied, failed) =
                            operations.values().fold((0, 0, 0, 0), |sum, value| {
                                (
                                    sum.0 + value.hits,
                                    sum.1 + value.effects,
                                    sum.2 + value.denied,
                                    sum.3 + value.failed,
                                )
                            });
                        lines.push(format!("{path}: {hits} hits, {effects} effects, {denied} denied, {failed} failed"));
                    }
                }
                lines
            }
            2 => vec![
                format!("Allowed: {net_allowed}    Denied: {net_denied}    Failed: {net_failed}"),
                format!(
                    "Boundary: {}",
                    snapshot
                        .record
                        .as_ref()
                        .and_then(|record| record.network_interception.as_ref())
                        .map_or("pending".into(), |value| format!(
                            "{:?} / {:?}",
                            value.driver, value.strength
                        ))
                ),
                format!(
                    "Policy: {}",
                    snapshot
                        .record
                        .as_ref()
                        .map_or("pending".into(), |record| record.network.to_string())
                ),
            ],
            _ => vec![
                format!(
                    "Run: {}",
                    snapshot
                        .record
                        .as_ref()
                        .map_or("pending", |record| record.run_id.as_str())
                ),
                format!(
                    "Agent: {}",
                    snapshot
                        .record
                        .as_ref()
                        .map_or("pending", |record| record.agent.as_str())
                ),
                format!(
                    "PID: {}",
                    snapshot.record.as_ref().map_or(0, |record| record.pid)
                ),
                format!(
                    "Command: {}",
                    snapshot
                        .record
                        .as_ref()
                        .map_or("pending".into(), |record| record.command.join(" "))
                ),
            ],
        };
        for line in top + 1..=size.ws_row {
            let index = (line - top - 1) as usize + scroll;
            row(
                stdout,
                line,
                size.ws_col,
                lines.get(index).map_or("", String::as_str),
                false,
            )?;
        }
    }
    write!(stdout, "\x1b8")?;
    stdout.flush()?;
    Ok(())
}

pub(super) fn run(args: Vec<OsString>) -> Result<i32> {
    let temporary = tempfile::tempdir()?;
    let stage_file = temporary.path().join("stage");
    let size = terminal_size();
    let mut master = -1;
    let mut slave = -1;
    let mut pty_size = size;
    pty_size.ws_row = child_rows(size, false);
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &pty_size,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("create Agent PTY");
    }
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut child = Command::new(std::env::current_exe()?);
    child
        .args(&args[1..])
        .env(CHILD_MARKER, "1")
        .env(STAGE_FILE, &stage_file)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?));
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ChildCleanup(child.spawn().context("start pVisor Run in PTY")?);
    drop(slave);
    let _terminal = TerminalGuard::raw()?;
    let mut stdout = std::io::stdout().lock();
    let mut stdin = std::io::stdin().lock();
    let mut size = size;
    let mut details = false;
    let mut tab = 0usize;
    let mut scroll = 0usize;
    let mut snapshot = Snapshot::default();
    let started = Instant::now();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut last_refresh = Instant::now() - Duration::from_secs(1);
    let mut exited = None;
    let mut drained_at = None;
    loop {
        let next_size = terminal_size();
        if next_size.ws_row != size.ws_row || next_size.ws_col != size.ws_col {
            size = next_size;
            resize_pty(&master, size, details);
            last_draw = Instant::now() - Duration::from_secs(1);
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            snapshot.refresh(&stage_file);
            last_refresh = Instant::now();
        }
        if last_draw.elapsed() >= Duration::from_millis(350) {
            draw(&mut stdout, size, details, tab, scroll, &snapshot, started)?;
            last_draw = Instant::now();
        }
        if exited.is_none() {
            exited = child.0.try_wait()?;
        }
        if exited.is_some() && drained_at.is_none() {
            drained_at = Some(Instant::now());
        }
        if drained_at.is_some_and(|at: Instant| at.elapsed() > Duration::from_millis(250)) {
            break;
        }
        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 80) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
            continue;
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let mut bytes = [0; 8192];
            match (&master).read(&mut bytes) {
                Ok(0) => {}
                Ok(count) => {
                    stdout.write_all(&bytes[..count])?;
                    stdout.flush()?;
                    if details {
                        last_draw = Instant::now() - Duration::from_secs(1);
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0; 1024];
            let count = stdin.read(&mut bytes)?;
            let mut forward = Vec::with_capacity(count);
            for byte in &bytes[..count] {
                if *byte == HOTKEY {
                    if details {
                        clear_panel(&mut stdout, size)?;
                    }
                    details = !details;
                    scroll = 0;
                    resize_pty(&master, size, details);
                    last_draw = Instant::now() - Duration::from_secs(1);
                } else if details {
                    match *byte {
                        b'\t' => { tab = (tab + 1) % 4; scroll = 0; },
                        b'1'..=b'4' => { tab = (byte - b'1') as usize; scroll = 0; },
                        b'j' => scroll = scroll.saturating_add(1),
                        b'k' => scroll = scroll.saturating_sub(1),
                        0x1b | b'q' => {
                            clear_panel(&mut stdout, size)?;
                            details = false;
                            resize_pty(&master, size, false);
                        }
                        _ => {}
                    }
                    last_draw = Instant::now() - Duration::from_secs(1);
                } else {
                    forward.push(*byte);
                }
            }
            if !forward.is_empty() {
                (&master).write_all(&forward)?;
            }
        }
    }
    if details {
        clear_panel(&mut stdout, size)?;
    }
    write!(stdout, "\x1b[{};1H\x1b[2K", size.ws_row)?;
    stdout.flush()?;
    let status = exited.unwrap_or(child.0.wait()?);
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

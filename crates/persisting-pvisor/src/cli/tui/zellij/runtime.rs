//! PTY ownership and event loop for the native pane UI.

use super::{
    audit_ui::{self, AuditServer, Scope, SessionPolicy},
    input, view,
};
use crate::runtime::{RunRecord, control_observations};
use anyhow::{Context, Result};
use persisting_control::audit::{AuditDecision, AuditRequest};
use persisting_control::ir::run::FilesystemObservation;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CHILD_MARKER: &str = "PVISOR_UI_CHILD";
const STAGE_FILE: &str = "PVISOR_UI_STAGE_FILE";
const LOG_FILE: &str = "PVISOR_UI_LOG_FILE";
const AUDIT_SOCKET: &str = "PVISOR_UI_AUDIT_SOCKET";
static CHILD_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();
static LOG_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(crate) fn init_child_context() {
    let path = if std::env::var_os(CHILD_MARKER).is_some() {
        std::env::var_os(STAGE_FILE).map(PathBuf::from)
    } else {
        None
    };
    let log_path = if path.is_some() {
        std::env::var_os(LOG_FILE).map(PathBuf::from)
    } else {
        None
    };
    let audit_socket = if path.is_some() {
        std::env::var_os(AUDIT_SOCKET).map(PathBuf::from)
    } else {
        None
    };
    // This runs before the Tokio runtime and any Agent environment is built.
    unsafe {
        std::env::remove_var(CHILD_MARKER);
        std::env::remove_var(STAGE_FILE);
        std::env::remove_var(LOG_FILE);
        std::env::remove_var(AUDIT_SOCKET);
    }
    if let Some(socket) = audit_socket {
        persisting_control::audit::init(socket);
    }
    let _ = CHILD_CONTEXT.set(path);
    let _ = LOG_CONTEXT.set(log_path);
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

pub(crate) fn announce_stage(stage: &Path) {
    if let Some(Some(path)) = CHILD_CONTEXT.get() {
        let _ = std::fs::write(path, stage.as_os_str().as_encoded_bytes());
    }
}

pub(crate) fn available() -> bool {
    CHILD_CONTEXT.get().is_some_and(Option::is_none)
        && unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1
        }
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

pub(crate) fn is_child() -> bool {
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
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(
            b"\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l",
        );
        let _ = stdout.flush();
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
pub(super) struct Snapshot {
    pub(super) stage: Option<PathBuf>,
    pub(super) record: Option<RunRecord>,
    pub(super) filesystem: Option<FilesystemObservation>,
    pub(super) network: Option<serde_json::Value>,
    pub(super) log: Vec<String>,
    pub(super) audit: Option<AuditRequest>,
    pub(super) audit_rules: Vec<String>,
}

impl Snapshot {
    fn refresh(&mut self, stage_file: &Path, log_file: &Path) {
        if let Ok(contents) = std::fs::read_to_string(log_file) {
            self.log = contents.lines().map(str::to_owned).collect();
        }
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

    pub(super) fn file_totals(&self) -> (u64, u64, u64, u64) {
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

    pub(super) fn network_totals(&self) -> (u64, u64, u64) {
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

fn resize_pty(master: &File, layout: view::Layout) {
    let size = libc::winsize {
        ws_row: layout.agent_rows,
        ws_col: layout.agent_cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
}

fn persist_audit_decision(
    storage: &Path,
    request: &AuditRequest,
    decision: AuditDecision,
    scope: Scope,
    automatic: bool,
) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(storage.join("audit.jsonl"))
        .context("open Job audit journal")?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "Job audit journal is not a regular file"
    );
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "at_unix_ms": timestamp,
            "request": request,
            "decision": decision,
            "scope": scope,
            "automatic": automatic,
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_data().context("sync Job audit journal")?;
    Ok(())
}

#[derive(Default)]
struct HostInputModes {
    paste: bool,
    cursor: bool,
    keypad: bool,
}

fn sync_input_modes(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    ui_owns_input: bool,
    modes: &mut HostInputModes,
) -> Result<()> {
    let paste = !ui_owns_input && screen.bracketed_paste();
    let cursor = !ui_owns_input && screen.application_cursor();
    let keypad = !ui_owns_input && screen.application_keypad();
    if modes.paste != paste {
        stdout.write_all(if paste {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        })?;
        modes.paste = paste;
    }
    if modes.cursor != cursor {
        stdout.write_all(if cursor { b"\x1b[?1h" } else { b"\x1b[?1l" })?;
        modes.cursor = cursor;
    }
    if modes.keypad != keypad {
        stdout.write_all(if keypad { b"\x1b=" } else { b"\x1b>" })?;
        modes.keypad = keypad;
    }
    Ok(())
}

pub(crate) fn run(args: Vec<OsString>, audit_enabled: bool) -> Result<i32> {
    let temporary = tempfile::tempdir()?;
    let stage_file = temporary.path().join("stage");
    let log_file = temporary.path().join("diagnostics.log");
    let audit_socket = temporary.path().join("audit.sock");
    let mut audit = audit_enabled
        .then(|| AuditServer::bind(&audit_socket))
        .transpose()?;
    File::create(&log_file).context("create TUI log")?;
    let mut size = terminal_size();
    let mut state = input::UiState::default();
    let mut layout = view::Layout::new(size, &state);
    let mut master = -1;
    let mut slave = -1;
    let pty_size = libc::winsize {
        ws_row: layout.agent_rows,
        ws_col: layout.agent_cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
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
        .env(LOG_FILE, &log_file)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?));
    if audit_enabled {
        child.env(AUDIT_SOCKET, &audit_socket);
    }
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
    let terminal = TerminalGuard::raw()?;
    let mut stdout = std::io::stdout().lock();
    let mut stdin = std::io::stdin().lock();
    // pVisor owns the outer alternate screen. Nested alternate screens from
    // Codex or other Agents stay inside the virtual terminal parser.
    stdout.write_all(b"\x1b[?1049h\x1b[?7l\x1b[2J\x1b[H")?;
    stdout.flush()?;
    let mut parser = vt100::Parser::new(layout.agent_rows, layout.agent_cols, 2000);
    let mut input_modes = HostInputModes::default();
    let mut snapshot = Snapshot::default();
    let mut audit_policy = SessionPolicy::default();
    let mut audit_policy_loaded = false;
    let started = Instant::now();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut last_refresh = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;
    let mut exited = None;
    let mut drained_at = None;
    loop {
        if let Some(audit) = audit.as_mut()
            && audit.poll()?
        {
            snapshot.refresh(&stage_file, &log_file);
            sync_input_modes(&mut stdout, parser.screen(), true, &mut input_modes)?;
            dirty = true;
        }
        if audit.is_some()
            && !audit_policy_loaded
            && let Some(record) = snapshot.record.as_ref()
        {
            audit_policy =
                SessionPolicy::load(&record.storage).context("load Job session audit policy")?;
            audit_policy_loaded = true;
        }
        if let Some(server) = audit.as_mut() {
            while let Some(request) = server.active().cloned() {
                let Some((decision, scope)) = audit_policy.resolve(&request) else {
                    break;
                };
                if let Some(record) = snapshot.record.as_ref() {
                    let _ =
                        persist_audit_decision(&record.storage, &request, decision, scope, true);
                }
                let _ = server.decide(decision);
                dirty = true;
            }
        }
        snapshot.audit_rules = audit_policy.rule_labels();
        snapshot.audit = audit.as_ref().and_then(AuditServer::active).cloned();
        let next_size = terminal_size();
        if next_size.ws_row != size.ws_row || next_size.ws_col != size.ws_col {
            size = next_size;
            layout = view::Layout::new(size, &state);
            parser
                .screen_mut()
                .set_size(layout.agent_rows, layout.agent_cols);
            resize_pty(&master, layout);
            dirty = true;
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            snapshot.refresh(&stage_file, &log_file);
            last_refresh = Instant::now();
            dirty = true;
        }
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            view::render(
                &mut stdout,
                layout,
                &state,
                parser.screen(),
                &snapshot,
                started,
            )?;
            last_draw = Instant::now();
            dirty = false;
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
                    parser.process(&bytes[..count]);
                    sync_input_modes(
                        &mut stdout,
                        parser.screen(),
                        !state.agent_input_active() || snapshot.audit.is_some(),
                        &mut input_modes,
                    )?;
                    // The Agent screen and the pVisor frame are always drawn
                    // together, so Agent cursor/clear sequences cannot touch
                    // the Review pane or the host shell prompt.
                    dirty = true;
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
                if let Some(server) = audit.as_mut()
                    && server.active().is_some()
                {
                    if let Some((scope, decision)) = server
                        .active()
                        .and_then(|request| audit_ui::choice(request, *byte))
                        && let Some(request) = server.active().cloned()
                    {
                        let next = audit_policy.with_decision(&request, scope, decision);
                        let save = snapshot
                            .record
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("Job record is not ready"))
                            .and_then(|record| {
                                next.as_ref()
                                    .ok_or_else(|| anyhow::anyhow!("invalid audit scope"))?
                                    .persist(&record.storage)
                            });
                        let actual = if save.is_ok() {
                            decision
                        } else {
                            AuditDecision::Deny
                        };
                        if let (Ok(()), Some(next)) = (&save, next) {
                            audit_policy = next;
                        }
                        let _ = server.decide(actual);
                        let mut file = OpenOptions::new().append(true).open(&log_file)?;
                        if let Err(error) = save {
                            writeln!(file, "audit policy unavailable; access denied: {error:#}")?;
                        }
                        if let Some(record) = snapshot.record.as_ref()
                            && let Err(error) = persist_audit_decision(
                                &record.storage,
                                &request,
                                actual,
                                scope,
                                false,
                            )
                        {
                            writeln!(file, "audit journal unavailable: {error:#}")?;
                        }
                        writeln!(
                            file,
                            "audit {:?} {:?}: {:?} {:?} ({:?})",
                            actual, scope, request.kind, request.target, request.reason
                        )?;
                        snapshot.audit_rules = audit_policy.rule_labels();
                        snapshot.audit = server.active().cloned();
                    }
                    dirty = true;
                    continue;
                }
                if let Some(byte) = state.input(*byte) {
                    forward.push(byte);
                } else {
                    dirty = true;
                }
            }
            if !forward.is_empty() {
                (&master).write_all(&forward)?;
            }
            sync_input_modes(
                &mut stdout,
                parser.screen(),
                !state.agent_input_active() || snapshot.audit.is_some(),
                &mut input_modes,
            )?;
        }
    }
    let status = exited.unwrap_or(child.0.wait()?);
    // Leave the outer alternate screen before printing the persistent review
    // location: the Run pane itself disappears with the TUI.
    let review_path = snapshot.stage.clone();
    drop(stdout);
    drop(terminal);
    if let Some(path) = review_path {
        eprintln!("Review: pvisor status --review {}", path.display());
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_follows_agent_focus() {
        let mut parser = vt100::Parser::new(12, 80, 0);
        parser.process(b"\x1b[?2004h");
        let mut output = Vec::new();
        let mut modes = HostInputModes::default();
        sync_input_modes(&mut output, parser.screen(), false, &mut modes).unwrap();
        assert_eq!(output, b"\x1b[?2004h");
        output.clear();
        sync_input_modes(&mut output, parser.screen(), true, &mut modes).unwrap();
        assert_eq!(output, b"\x1b[?2004l");
    }
}

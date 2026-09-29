//! PTY ownership and event loop for the native pane UI.

use super::{
    audit_ui::{self, AuditServer, Lifetime, Permissions, Prompt, Scope},
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
    crate::cache::progress::init_output(
        log_path
            .as_ref()
            .map(|path| path.with_extension("image.json")),
    );
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
    pub(super) image: Option<crate::cache::progress::ImageProgress>,
    pub(super) stage: Option<PathBuf>,
    pub(super) record: Option<RunRecord>,
    pub(super) filesystem: Option<FilesystemObservation>,
    pub(super) network: Option<serde_json::Value>,
    pub(super) log: Vec<String>,
    pub(super) audit: Option<AuditRequest>,
    pub(super) audit_rules: Vec<String>,
    pub(super) audit_prompt: Prompt,
}

impl Snapshot {
    fn refresh(&mut self, stage_file: &Path, log_file: &Path) {
        if let Ok(bytes) = std::fs::read(log_file.with_extension("image.json"))
            && let Ok(image) = serde_json::from_slice(&bytes)
        {
            self.image = Some(image);
        }
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
            number("policy_denied") + number("tcp_flows_denied"),
            number("failures") + number("tcp_connect_failures"),
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
    lifetime: Lifetime,
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
            "lifetime": lifetime,
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
    let mut pty_size = libc::winsize {
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
            std::ptr::null_mut(),
            &raw mut pty_size,
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
            #[cfg(target_os = "linux")]
            let tiocsctty = libc::TIOCSCTTY;
            #[cfg(not(target_os = "linux"))]
            let tiocsctty: libc::c_ulong = libc::TIOCSCTTY.into();
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, tiocsctty, 0) < 0 {
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
    let mut review_mouse = false;
    let mut snapshot = Snapshot::default();
    let mut audit_policy: Option<Permissions> = None;
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
            && audit_policy.is_none()
            && let Some(record) = snapshot.record.as_ref()
        {
            let workspace = record.workspace.clone().unwrap_or(std::env::current_dir()?);
            let file_root = record
                .overlay
                .as_ref()
                .map(|o| o.target.as_path())
                .unwrap_or(&workspace);
            audit_policy = Some(
                Permissions::load(
                    &record.storage,
                    &workspace,
                    file_root,
                    audit_ui::permissions_config_path()?,
                )
                .context("load audit permissions")?,
            );
        }
        if let Some(server) = audit.as_mut() {
            while let Some(request) = server.active().cloned() {
                let Some((decision, scope, lifetime)) =
                    audit_policy.as_ref().and_then(|p| p.resolve(&request))
                else {
                    break;
                };
                if let Some(record) = snapshot.record.as_ref() {
                    let _ = persist_audit_decision(
                        &record.storage,
                        &request,
                        decision,
                        scope,
                        true,
                        lifetime,
                    );
                }
                let _ = server.decide(decision);
                snapshot.audit_prompt = Prompt::default();
                dirty = true;
            }
        }
        snapshot.audit_rules = audit_policy
            .as_ref()
            .map(Permissions::rule_labels)
            .unwrap_or_default();
        snapshot.audit = audit.as_ref().and_then(AuditServer::active).map(|r| {
            audit_policy
                .as_ref()
                .map_or_else(|| r.clone(), |p| p.display_request(r))
        });
        state.permission = state
            .permission
            .min(snapshot.audit_rules.len().saturating_sub(1));
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
        dirty |= state.expire_escape(Instant::now());
        let mouse = state.panel_open() && snapshot.audit.is_none();
        if mouse != review_mouse {
            stdout.write_all(if mouse {
                b"\x1b[?1000h\x1b[?1006h"
            } else {
                b"\x1b[?1000l\x1b[?1006l"
            })?;
            review_mouse = mouse;
            dirty = true;
        }
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            view::render(
                &mut stdout,
                layout,
                &mut state,
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
                    if (size.ws_col < 60 || size.ws_row < 16)
                        && !matches!(*byte, b'd' | b'D' | 0x1b)
                    {
                        continue;
                    }
                    if let Some((scope, decision)) = server
                        .active()
                        .and_then(|request| snapshot.audit_prompt.input(request, *byte))
                        && let Some(request) = server.active().cloned()
                    {
                        let lifetime = snapshot.audit_prompt.lifetime;
                        let save = snapshot
                            .record
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("Job record is not ready"))
                            .and_then(|record| {
                                audit_policy
                                    .as_mut()
                                    .context("audit permissions are not loaded")?
                                    .remember(&record.storage, &request, scope, decision, lifetime)
                            });
                        let actual = if save.is_ok() {
                            decision
                        } else {
                            AuditDecision::Deny
                        };
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
                                lifetime,
                            )
                        {
                            writeln!(file, "audit journal unavailable: {error:#}")?;
                        }
                        writeln!(
                            file,
                            "audit {:?} {:?}: {:?} {:?} ({:?})",
                            actual, scope, request.kind, request.target, request.reason
                        )?;
                        snapshot.audit_rules = audit_policy
                            .as_ref()
                            .map(Permissions::rule_labels)
                            .unwrap_or_default();
                        snapshot.audit = server.active().cloned();
                        snapshot.audit_prompt = Prompt::default();
                    }
                    dirty = true;
                    continue;
                }
                if state.panel_open() && state.panel == input::Panel::Permissions && *byte == b'x' {
                    if state.forget_pending {
                        let result = snapshot
                            .record
                            .as_ref()
                            .context("Job record is not ready")
                            .and_then(|record| {
                                audit_policy
                                    .as_mut()
                                    .context("Permissions are not loaded")?
                                    .forget(&record.storage, state.permission)
                            });
                        if let Err(error) = result {
                            writeln!(
                                OpenOptions::new().append(true).open(&log_file)?,
                                "Cannot forget permission: {error:#}"
                            )?;
                        }
                        snapshot.audit_rules = audit_policy
                            .as_ref()
                            .map(Permissions::rule_labels)
                            .unwrap_or_default();
                    }
                    state.forget_pending = !state.forget_pending;
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
    snapshot.refresh(&stage_file, &log_file);
    // Leave the outer alternate screen before printing the persistent review
    // location: the Run pane itself disappears with the TUI.
    drop(stdout);
    drop(terminal);
    write_exit_report(
        &mut std::io::stderr().lock(),
        status,
        &snapshot,
        parser.screen(),
    )?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

fn write_exit_report(
    output: &mut impl Write,
    status: std::process::ExitStatus,
    snapshot: &Snapshot,
    screen: &vt100::Screen,
) -> std::io::Result<()> {
    if !status.success() {
        for line in &snapshot.log {
            writeln!(output, "{line}")?;
        }
        let contents = screen.contents();
        if !contents.trim().is_empty() {
            writeln!(output, "{contents}")?;
        }
        writeln!(output, "pVisor Job failed: {status}")?;
    }
    if let Some(path) = &snapshot.stage {
        writeln!(output, "Review: pvisor status --review {}", path.display())?;
        if snapshot.record.as_ref().is_some_and(|r| {
            r.overlay
                .as_ref()
                .is_some_and(|o| !o.auto_discard && !o.auto_apply)
        }) {
            writeln!(
                output,
                "Changes retained. Apply: pvisor apply {} | Discard: pvisor drop {}",
                path.display(),
                path.display()
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_transfers_reach_log_panel_without_throttling() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("diagnostics.log");
        File::create(&log).unwrap();
        LOG_CONTEXT.set(Some(log.clone())).unwrap();
        let downloads = crate::cache::progress::Downloads::new("example:latest");
        downloads.received(b"transfer-log-test/file\nname", 10);
        downloads.received(b"transfer-log-test/file\nname", 20);
        let mut snapshot = Snapshot::default();
        snapshot.refresh(&directory.path().join("missing"), &log);
        let transfers: Vec<_> = snapshot
            .log
            .iter()
            .filter(|line| line.contains("transfer-log-test"))
            .collect();
        assert_eq!(transfers.len(), 2);
        assert!(transfers[0].contains("transferred 10 bytes from /transfer-log-test/file\\nname"));
        assert!(transfers[1].contains("this run: 30 bytes across 1 files"));
    }

    #[test]
    fn image_progress_is_visible_before_stage_is_announced() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("diagnostics.log");
        let path = log.with_extension("image.json");
        std::fs::write(&path, br#"{"image":"ubuntu:latest","totals":{"files":100,"bytes":2000},"downloaded_files":2,"downloaded_bytes":10}"#).unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.refresh(&directory.path().join("missing"), &log);
        assert!(snapshot.stage.is_none());
        assert_eq!(snapshot.image.as_ref().unwrap().downloaded_files, 2);
        std::fs::write(&path, b"invalid").unwrap();
        snapshot.refresh(&directory.path().join("missing"), &log);
        assert_eq!(snapshot.image.as_ref().unwrap().downloaded_bytes, 10);
    }

    #[test]
    fn fast_failure_preserves_diagnostics_and_review_location() {
        let temporary = tempfile::tempdir().unwrap();
        let stage_file = temporary.path().join("stage");
        let log_file = temporary.path().join("diagnostics.log");
        std::fs::write(&stage_file, b"/missing/run").unwrap();
        std::fs::write(&log_file, b"resolve Agent executable: missing\n").unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.refresh(&stage_file, &log_file);
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Error: startup failed\r\n");
        let mut output = Vec::new();
        write_exit_report(
            &mut output,
            std::process::ExitStatus::from_raw(1 << 8),
            &snapshot,
            parser.screen(),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("resolve Agent executable: missing"));
        assert!(output.contains("Error: startup failed"));
        assert!(output.contains("pVisor Job failed: exit status: 1"));
        assert!(output.contains("Review: pvisor status --review /missing/run"));
    }

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

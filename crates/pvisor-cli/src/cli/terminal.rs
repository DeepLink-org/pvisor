//! Small bridge between the core Job and its external terminal frontend.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const CHILD_MARKER: &str = "PVISOR_UI_CHILD";
pub const STAGE_FILE: &str = "PVISOR_UI_STAGE_FILE";
pub const LOG_FILE: &str = "PVISOR_UI_LOG_FILE";
pub const AUDIT_SOCKET: &str = "PVISOR_UI_AUDIT_SOCKET";
static CHILD_CONTEXT: OnceLock<Option<PathBuf>> = OnceLock::new();
static SERVICE_CONTEXT: OnceLock<Option<ServiceTerminalContext>> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ServiceTerminalContext {
    stage_file: PathBuf,
    log_file: Option<PathBuf>,
    audit_socket: Option<PathBuf>,
}

pub(crate) fn service_context() -> Option<ServiceTerminalContext> {
    SERVICE_CONTEXT.get().cloned().flatten()
}

/// Called only in the fresh, single-threaded host worker before initialization.
pub(crate) fn restore_service_context(context: Option<ServiceTerminalContext>) {
    if let Some(context) = context {
        unsafe {
            std::env::set_var(CHILD_MARKER, "1");
            std::env::set_var(STAGE_FILE, context.stage_file);
            if let Some(path) = context.log_file {
                std::env::set_var(LOG_FILE, path);
            }
            if let Some(path) = context.audit_socket {
                std::env::set_var(AUDIT_SOCKET, path);
            }
        }
    }
}

pub fn init_child_context() {
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
    let _ = SERVICE_CONTEXT.set(path.as_ref().map(|stage_file| ServiceTerminalContext {
        stage_file: stage_file.clone(),
        log_file: log_path.clone(),
        audit_socket: audit_socket.clone(),
    }));
    // This runs before the Tokio runtime and any Agent environment is built.
    unsafe {
        std::env::remove_var(CHILD_MARKER);
        std::env::remove_var(STAGE_FILE);
        std::env::remove_var(LOG_FILE);
        std::env::remove_var(AUDIT_SOCKET);
    }
    if let Some(socket) = audit_socket {
        pvisor::audit::init(socket);
    }
    let _ = CHILD_CONTEXT.set(path);
    pvisor::cache::progress::init_output(
        log_path
            .as_ref()
            .map(|path| path.with_extension("image.json")),
    );
    pvisor::diagnostics::init(log_path);
}

pub fn announce_stage(stage: &Path) {
    if let Some(Some(path)) = CHILD_CONTEXT.get() {
        let _ = std::fs::write(path, stage.as_os_str().as_encoded_bytes());
    }
}

pub fn available() -> bool {
    CHILD_CONTEXT.get().is_some_and(Option::is_none)
        && unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1
        }
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

pub fn is_child() -> bool {
    CHILD_CONTEXT.get().is_some_and(Option::is_some)
}

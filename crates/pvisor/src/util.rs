//! Shared helpers for pVisor.

pub use pvisor_core::unix_now_ms;
#[cfg(test)]
use std::fs;
use std::path::Path;

pub(crate) use pvisor_journal::{create_dir_all_durable, sync_directory};

pub(crate) use pvisor_journal::atomic_write;

/// Publish owner-only JSON using the same durable replacement as Run records.
pub fn write_private_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?, 0o600)
}

/// Persistence diagnostics carry durations, not additional startup checkpoints.
pub(crate) fn persistence_log(
    run_id: &str,
    object: &str,
    phase: &str,
    elapsed: std::time::Duration,
    success: bool,
) {
    if !startup_logging_enabled() {
        return;
    }
    crate::diagnostics::diagnostic(format_args!(
        "pvisor-persistence level=info timestamp_ms={} pid={} run_id={} object={} phase={} duration_us={} outcome={}",
        unix_now_ms(),
        std::process::id(),
        serde_json::to_string(run_id).unwrap_or_default(),
        object,
        phase,
        elapsed.as_micros(),
        if success { "ok" } else { "error" }
    ));
}

pub(crate) fn persistence_step<T, E>(
    run_id: &str,
    object: &str,
    phase: &str,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let start = std::time::Instant::now();
    let result = operation();
    persistence_log(run_id, object, phase, start.elapsed(), result.is_ok());
    result
}

pub(crate) fn write_run_json(
    path: &Path,
    value: &impl serde::Serialize,
    run_id: &str,
    object: &str,
) -> anyhow::Result<()> {
    let body = persistence_step(run_id, object, "serialize", || {
        serde_json::to_vec_pretty(value)
    })?;
    write_run_bytes(path, &body, run_id, object)
}

pub(crate) fn write_run_bytes(
    path: &Path,
    body: &[u8],
    run_id: &str,
    object: &str,
) -> anyhow::Result<()> {
    pvisor_journal::atomic_write_observed(path, body, 0o600, |phase, elapsed, success| {
        persistence_log(run_id, object, phase, elapsed, success);
    })
}

fn startup_logging_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PVISOR_STARTUP_TIMING").as_deref() != Ok("0"))
}

/// Routine host checkpoints; guest clocks have a different epoch.
/// `PVISOR_STARTUP_TIMING=0` suppresses output for uninstrumented benchmarks.
pub fn startup_mark(stage: &str) {
    startup_checkpoint(stage, None);
}

pub fn startup_mark_run(stage: &str, run_id: &str) {
    startup_checkpoint(stage, Some(run_id));
}

/// Host work counters accompanying startup/lifecycle timing, without guest data.
pub(crate) fn startup_detail_run(stage: &str, run_id: &str, fields: std::fmt::Arguments<'_>) {
    if startup_logging_enabled() {
        crate::diagnostics::diagnostic(format_args!(
            "pvisor-startup-detail level=info pid={} run_id={} stage={} {}",
            std::process::id(),
            serde_json::to_string(run_id).unwrap_or_default(),
            stage,
            fields,
        ));
    }
}

fn startup_checkpoint(stage: &str, run_id: Option<&str>) {
    #[cfg(unix)]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        if !startup_logging_enabled() {
            return;
        }
        if let Some(timestamp) = startup_monotonic_us() {
            crate::diagnostics::diagnostic(format_args!(
                "pvisor-startup level=info timestamp_ms={} pid={} ppid={} run_id={} stage={} monotonic_us={} process_elapsed_us={}",
                unix_now_ms(),
                std::process::id(),
                unsafe { libc::getppid() },
                serde_json::to_string(run_id.unwrap_or("-")).unwrap_or_default(),
                stage,
                timestamp,
                start.elapsed().as_micros()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (stage, run_id);
}

#[cfg(unix)]
fn startup_monotonic_us() -> Option<u64> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return None;
    }
    Some(
        u64::try_from(timestamp.tv_sec)
            .ok()?
            .checked_mul(1_000_000)?
            + u64::try_from(timestamp.tv_nsec).ok()? / 1_000,
    )
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn startup_clock_is_monotonic() {
        let before = startup_monotonic_us().expect("host monotonic clock");
        let after = startup_monotonic_us().expect("host monotonic clock");
        assert!(after >= before);
    }

    #[test]
    fn atomic_write_replaces_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record.json");
        atomic_write(&path, b"first", 0o600).unwrap();
        atomic_write(&path, b"second", 0o600).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn private_json_preserves_previous_contents_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/result.json");
        write_private_json(&path, &serde_json::json!({"state": "completed"})).unwrap();
        let original = fs::read(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let invalid = std::collections::BTreeMap::from([(vec![1, 2], "invalid JSON key")]);
        assert!(write_private_json(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);

        let directory = temp.path().join("existing-directory");
        fs::create_dir(&directory).unwrap();
        assert!(write_private_json(&directory, &true).is_err());
        assert!(directory.is_dir());
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            2,
            "temporary file leaked"
        );
    }

    #[test]
    fn durable_directory_creation_handles_nested_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("one/two/three");

        create_dir_all_durable(&path).unwrap();
        assert!(path.is_dir());
    }
}

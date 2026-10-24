//! Opt-in host startup diagnostics. Unknown outcomes include errors and cancellation.
use serde::Serialize;
use std::{
    io::{self, Write},
    sync::OnceLock,
    time::Instant,
};

const PREFIX: &[u8] = b"pvisor-startup-profile ";
const MAX_LINE: usize = 768;
static ENABLED: OnceLock<bool> = OnceLock::new();
// Local monotonic epoch begins at the first enabled diagnostic in this process.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

struct Timing {
    origin: Instant,
    start: Instant,
}

pub(crate) struct Span {
    stage: &'static str,
    timing: Option<Timing>,
    outcome: &'static str,
    bytes: Option<u64>,
}

impl Span {
    pub(crate) fn start(stage: &'static str) -> Self {
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var_os("PVISOR_STARTUP_PROFILE").as_deref() == Some(std::ffi::OsStr::new("1"))
        });
        Self::start_with(stage, enabled, || {
            let origin = *ORIGIN.get_or_init(Instant::now);
            Timing {
                origin,
                start: Instant::now(),
            }
        })
    }

    fn start_with(stage: &'static str, enabled: bool, clock: impl FnOnce() -> Timing) -> Self {
        Self {
            stage,
            timing: enabled.then(clock),
            outcome: "unknown",
            bytes: None,
        }
    }

    pub(crate) fn success(&mut self, bytes: Option<u64>) {
        self.outcome = "success";
        self.bytes = bytes;
    }
}

#[derive(Serialize)]
struct Record {
    schema: u8,
    pid: u32,
    role: &'static str,
    epoch: &'static str,
    clock: &'static str,
    stage: &'static str,
    start_ns: u64,
    end_ns: u64,
    duration_ns: u64,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
}

impl Record {
    fn closed(
        stage: &'static str,
        start_ns: u64,
        end_ns: u64,
        outcome: &'static str,
        bytes: Option<u64>,
    ) -> Self {
        Self {
            schema: 1,
            pid: std::process::id(),
            role: "host",
            epoch: "runtime",
            clock: "host_monotonic_process_relative",
            stage,
            start_ns,
            end_ns,
            duration_ns: end_ns.saturating_sub(start_ns),
            outcome,
            bytes,
        }
    }

    fn emit(&self, writer: &mut impl Write) {
        let mut buffer = [0_u8; MAX_LINE];
        let mut line = io::Cursor::new(buffer.as_mut_slice());
        if line.write_all(PREFIX).is_err()
            || serde_json::to_writer(&mut line, self).is_err()
            || line.write_all(b"\n").is_err()
        {
            return;
        }
        let length = line.position() as usize;
        // Diagnostics must never replace the operation's error, including EAGAIN.
        let _ = writer.write_all(&buffer[..length]);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(timing) = &self.timing else {
            return;
        };
        let end = Instant::now();
        let ns = |instant: Instant| {
            instant
                .duration_since(timing.origin)
                .as_nanos()
                .min(u64::MAX as u128) as u64
        };
        Record::closed(
            self.stage,
            ns(timing.start),
            ns(end),
            self.outcome,
            self.bytes,
        )
        .emit(&mut DiagnosticWriter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_span_does_not_read_clock_or_emit() {
        let mut span = Span::start_with("disabled", false, || panic!("clock read while disabled"));
        assert!(span.timing.is_none());
        span.success(Some(42));
        drop(span);
    }

    #[test]
    fn closed_duration_serializes_as_bounded_single_line() {
        let mut output = Vec::new();
        Record::closed(
            "vm.checkpoint.embedded_kernel_sha256",
            17,
            59,
            "success",
            Some(123),
        )
        .emit(&mut output);
        assert!(output.len() <= MAX_LINE);
        assert!(output.starts_with(PREFIX));
        assert_eq!(output.iter().filter(|&&byte| byte == b'\n').count(), 1);
        let value: serde_json::Value = serde_json::from_slice(&output[PREFIX.len()..]).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["role"], "host");
        assert_eq!(value["clock"], "host_monotonic_process_relative");
        assert_eq!(value["start_ns"], 17);
        assert_eq!(value["end_ns"], 59);
        assert_eq!(value["duration_ns"], 42);
        assert_eq!(value["outcome"], "success");
        assert_eq!(value["bytes"], 123);
    }

    #[test]
    fn unknown_outcome_omits_unknown_bytes_and_logging_is_best_effort() {
        let record = Record::closed("vm.checkpoint.compatibility", 0, 1, "unknown", None);
        let mut output = Vec::new();
        record.emit(&mut output);
        let value: serde_json::Value = serde_json::from_slice(&output[PREFIX.len()..]).unwrap();
        assert_eq!(value["outcome"], "unknown");
        assert!(value.get("bytes").is_none());

        struct Unavailable;
        impl Write for Unavailable {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(libc::EAGAIN))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        record.emit(&mut Unavailable);
    }
}

struct DiagnosticWriter;
impl Write for DiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Ok(line) = std::str::from_utf8(bytes) {
            crate::diagnostics::diagnostic(format_args!("{}", line.trim_end_matches('\n')));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

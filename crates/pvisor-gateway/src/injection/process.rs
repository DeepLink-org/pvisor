//! Bounded best-effort metadata probes; never used for model or tool execution.
use std::io::{self, Read, Seek, SeekFrom};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(super) fn output(command: &mut Command) -> io::Result<Output> {
    output_with_timeout(command, Duration::from_secs(1))
}

fn output_with_timeout(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    // A file avoids pipe backpressure while polling/reaping the child. The
    // output is metadata only; cap what is retained in memory.
    let mut stdout = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10))
            }
            result => {
                // A kernel-uninterruptible child may delay wait() after SIGKILL.
                // Keep this background slot occupied until it is actually reaped;
                // no gateway request awaits this session.
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "client metadata probe timed out"),
                });
            }
        }
    };
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::other("client metadata probe output too large"));
    }
    Ok(Output {
        status,
        stdout: bytes,
        stderr: Vec::new(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn output_and_nonzero_status_are_preserved() {
        let out = output(Command::new("sh").args(["-c", "printf client; exit 7"])).unwrap();
        assert_eq!(out.stdout, b"client");
        assert_eq!(out.status.code(), Some(7));
    }
    #[test]
    fn timeout_kills_and_reaps_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let error = output_with_timeout(
            Command::new("sh")
                .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "probe"])
                .arg(&pid_file),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let pid = std::fs::read_to_string(pid_file).unwrap();
        assert!(
            !Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }
    #[test]
    fn large_output_does_not_deadlock_on_a_pipe() {
        let out = output(Command::new("sh").args(["-c", "head -c 131072 /dev/zero"])).unwrap();
        assert_eq!(out.stdout.len(), 131072);
    }
}

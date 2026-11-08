//! Owned self-executed reader for the two prepared-base lease protocols.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const DIAGNOSTIC_BYTES: usize = 4096;

// Match an exact newline-terminated marker without retaining a growing line.
// A mismatched/overlong line stays rejected until its newline arrives.
fn exact_marker(bytes: &[u8], marker: &[u8], matched: &mut usize, rejected: &mut bool) -> bool {
    for &byte in bytes {
        if byte == b'\n' {
            let ready = !*rejected && *matched == marker.len();
            *matched = 0;
            *rejected = false;
            if ready {
                return true;
            }
        } else if !*rejected && *matched < marker.len() && byte == marker[*matched] {
            *matched += 1;
        } else {
            *rejected = true;
        }
    }
    false
}

fn retain_tail(tail: &mut VecDeque<u8>, bytes: &[u8]) {
    for &byte in bytes {
        if tail.len() == DIAGNOSTIC_BYTES {
            tail.pop_front();
        }
        tail.push_back(byte);
    }
}

/// Keeps a prepared-base reader and its stdout drain alive until release.
pub(super) struct LeaseChild {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl LeaseChild {
    /// Spawns the supplied self-exec command and waits at most five seconds for
    /// its exact marker. Failures retain at most 4 KiB of raw stdout tail and
    /// dispose of owned resources. Only the two short ASCII markers are supported.
    pub(super) fn spawn(mut command: Command, marker: &str) -> Self {
        assert!(
            !marker.is_empty()
                && marker.len() <= 128
                && marker.bytes().all(|b| b.is_ascii_graphic())
        );
        let marker = marker.as_bytes().to_vec();
        let mut owned = Self {
            child: command
                // Pretty libtest output prefixes the marker without a newline.
                .args(["--format", "terse"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
            reader: None,
        };
        let mut output = owned.child.stdout.take().unwrap();
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(DIAGNOSTIC_BYTES)));
        let reader_tail = Arc::clone(&tail);
        // Exactly one notification: readiness or read failure. Noise never
        // enters a channel, so it cannot extend the wait or backpressure drain.
        let (ready_tx, ready_rx) = mpsc::channel();
        owned.reader = Some(thread::spawn(move || {
            let mut notification = Some(ready_tx);
            let mut chunk = [0; 1024];
            let mut matched = 0;
            let mut rejected = false;
            loop {
                match output.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        retain_tail(&mut reader_tail.lock().unwrap(), &chunk[..count]);
                        if notification.is_some()
                            && exact_marker(&chunk[..count], &marker, &mut matched, &mut rejected)
                        {
                            let _ = notification.take().unwrap().send(Ok(()));
                        }
                        // Continue draining after readiness, without sending
                        // further messages or retaining an unterminated line.
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        if let Some(sender) = notification.take() {
                            let _ = sender.send(Err(error));
                        }
                        break;
                    }
                }
            }
        }));
        // A single timed receive cannot be prolonged by queued noise.
        let ready = ready_rx.recv_timeout(Duration::from_secs(5));
        if !matches!(ready, Ok(Ok(()))) {
            let bytes: Vec<_> = tail.lock().unwrap().iter().copied().collect();
            panic!(
                "child lease readiness failed: {ready:?}; observed stdout tail: {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
        owned
    }

    /// Releases the reader with one byte and requires successful exit within
    /// five seconds. Drop also runs on write, exit or assertion failure.
    pub(super) fn release(mut self) {
        self.child.stdin.take().unwrap().write_all(b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "lease reader did not exit after release"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
    }
}

impl Drop for LeaseChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_matching_discards_noise_and_overlong_lines_across_chunks() {
        let marker = b"WORKSPACE_LEASE_READY";
        let mut matched = 0;
        let mut rejected = false;
        // Finite 8 KiB input, including an unterminated overlong line.
        for _ in 0..8 {
            assert!(!exact_marker(
                &[b'x'; 1024],
                marker,
                &mut matched,
                &mut rejected
            ));
            assert!(matched <= marker.len());
        }
        assert!(!exact_marker(marker, marker, &mut matched, &mut rejected));
        assert!(!exact_marker(b"\n", marker, &mut matched, &mut rejected));
        assert!(!exact_marker(
            b"test name ... WORKSPACE_LEASE_READY\n",
            marker,
            &mut matched,
            &mut rejected
        ));
        assert!(!exact_marker(
            b"WORKSPACE_",
            marker,
            &mut matched,
            &mut rejected
        ));
        assert!(exact_marker(
            b"LEASE_READY\n",
            marker,
            &mut matched,
            &mut rejected
        ));
    }

    #[test]
    fn diagnostic_tail_stays_capped_without_lines() {
        let mut tail = VecDeque::with_capacity(DIAGNOSTIC_BYTES);
        for _ in 0..8 {
            retain_tail(&mut tail, &[b'x'; 1024]);
            assert!(tail.len() <= DIAGNOSTIC_BYTES);
        }
        retain_tail(&mut tail, b"last bytes");
        assert_eq!(tail.len(), DIAGNOSTIC_BYTES);
        assert!(
            tail.iter()
                .copied()
                .collect::<Vec<_>>()
                .ends_with(b"last bytes")
        );
    }
}

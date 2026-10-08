//! Linux runtime control for pVisor's single-VM runner.
//! Commands execute after the whole event-manager batch returns. A successful
//! Pause cannot be followed by another device callback from that batch.
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use polly::event_manager::{EventManager, Subscriber};
use serde_json::{json, Value};
use utils::epoll::{EpollEvent, EventSet};

const MAX_REQUEST_BYTES: usize = 4096;
const VCPU_TIMEOUT: Duration = Duration::from_secs(3);
const DEVICE_TIMEOUT: Duration = Duration::from_secs(5);

/// Duplicate a connected AF_UNIX stream, retaining the caller's ownership.
pub fn duplicate_stream(fd: RawFd) -> io::Result<UnixStream> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(duplicate) };
    for (option, expected) in [
        (libc::SO_DOMAIN, libc::AF_UNIX),
        (libc::SO_TYPE, libc::SOCK_STREAM),
    ] {
        let mut value = 0i32;
        let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&mut value as *mut i32).cast(),
                &mut length,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if value != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control requires an AF_UNIX stream",
            ));
        }
    }
    stream.peer_addr()?;
    Ok(stream)
}

/// A runtime-controlled VM must be the sole VM started by this process.
pub fn register_vm_start(
    starts: &std::sync::atomic::AtomicUsize,
    controlled: bool,
) -> Result<(), ()> {
    use std::sync::atomic::Ordering;
    const CONTROLLED: usize = 1 << (usize::BITS - 1);
    starts
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |previous| {
            if previous & CONTROLLED != 0 || (controlled && previous != 0) {
                None
            } else if controlled {
                Some(CONTROLLED)
            } else {
                previous
                    .checked_add(1)
                    .filter(|next| next & CONTROLLED == 0)
            }
        })
        .map(|_| ())
        .map_err(|_| ())
}

struct WakeControl(RawFd);
impl Subscriber for WakeControl {
    fn process(&mut self, _: &EpollEvent, _: &mut EventManager) {
        // Leave readable; the outer loop consumes requests after this batch.
    }
    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(EventSet::IN, self.0 as u64)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running,
    Paused,
    Faulted,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Faulted => "faulted",
        }
    }
}

// Test the completion/failure contract without requiring /dev/kvm.
trait ControlledVm {
    fn pause_cpus(&mut self) -> Result<(), String>;
    fn resume_cpus(&mut self) -> Result<(), String>;
    fn pause_devices(&mut self) -> Result<(), String>;
    fn resume_devices(&mut self);
}
impl ControlledVm for vmm::Vmm {
    fn pause_cpus(&mut self) -> Result<(), String> {
        self.pause_vcpus(VCPU_TIMEOUT)
    }
    fn resume_cpus(&mut self) -> Result<(), String> {
        self.resume_vcpus_with_timeout(VCPU_TIMEOUT)
    }
    fn pause_devices(&mut self) -> Result<(), String> {
        devices::virtio::pause::pause(DEVICE_TIMEOUT)
    }
    fn resume_devices(&mut self) {
        devices::virtio::pause::resume();
    }
}

pub struct RuntimeControl {
    stream: UnixStream,
    input: Vec<u8>,
    state: State,
    fault: Option<String>,
}
impl RuntimeControl {
    pub fn new(stream: UnixStream, events: &mut EventManager) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        events
            .add_subscriber(Arc::new(Mutex::new(WakeControl(stream.as_raw_fd()))))
            .map_err(|e| io::Error::other(format!("register runtime control: {e:?}")))?;
        Ok(Self {
            stream,
            input: Vec::new(),
            state: State::Running,
            fault: None,
        })
    }
    /// Faulted can mean partially paused or in-flight I/O. Never call it Paused.
    pub fn runs_devices(&self) -> bool {
        self.state == State::Running
    }
    pub fn wait(&self) -> io::Result<()> {
        poll_fd(self.stream.as_raw_fd(), libc::POLLIN, -1)
    }
    pub fn process_pending(&mut self, vm: &Arc<Mutex<vmm::Vmm>>) -> io::Result<()> {
        while let Some(request) = self.read_request()? {
            let response = self.execute(&request, &mut *vm.lock().unwrap());
            self.write_response(&response)?;
        }
        Ok(())
    }
    fn execute(&mut self, request: &Value, vm: &mut impl ControlledVm) -> Value {
        let request_id = request.get("request_id").and_then(Value::as_u64);
        let command = request.get("command").and_then(Value::as_str);
        let result = if request_id.is_none() {
            Err("request_id must be an unsigned integer".to_string())
        } else if command == Some("status") {
            Ok(())
        } else if !matches!(command, Some("pause" | "resume")) {
            Err("command must be pause, resume, or status".to_string())
        } else if self.state == State::Faulted {
            Err(self
                .fault
                .clone()
                .unwrap_or_else(|| "VM is faulted; terminate the runner".into()))
        } else {
            let transition = match (command, self.state) {
                (Some("pause"), State::Running) => {
                    vm.pause_cpus().and_then(|()| vm.pause_devices())
                }
                (Some("resume"), State::Paused) => {
                    // Queue objects, eventfd counters and worker objects survive.
                    vm.resume_devices();
                    vm.resume_cpus()
                }
                _ => Ok(()), // Repeated pause/resume is idempotent.
            };
            match transition {
                Ok(()) => {
                    self.state = if command == Some("pause") {
                        State::Paused
                    } else {
                        State::Running
                    };
                    Ok(())
                }
                Err(error) => {
                    // Stale acknowledgements after a timeout make blind rollback
                    // or retry unsafe. Preserve failure; parent can terminate.
                    self.state = State::Faulted;
                    self.fault = Some(error.clone());
                    Err(error)
                }
            }
        };
        let error = result
            .as_ref()
            .err()
            .cloned()
            .or_else(|| self.fault.clone());
        json!({"request_id": request_id, "ok": result.is_ok(), "state": self.state.name(), "error": error})
    }
    fn read_request(&mut self) -> io::Result<Option<Value>> {
        loop {
            if let Some(end) = self.input.iter().position(|byte| *byte == b'\n') {
                if end > MAX_REQUEST_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "runtime request too large",
                    ));
                }
                let line: Vec<_> = self.input.drain(..=end).collect();
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
            }
            if self.input.len() > MAX_REQUEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runtime request too large",
                ));
            }
            let mut bytes = [0; 512];
            match self.stream.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "runtime controller disconnected",
                    ))
                }
                Ok(count) => self.input.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    fn write_response(&mut self, response: &Value) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(response)?;
        bytes.push(b'\n');
        let mut remaining = bytes.as_slice();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !remaining.is_empty() {
            match self.stream.write(remaining) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "runtime response write failed",
                    ))
                }
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "runtime response timed out",
                        ));
                    }
                    poll_fd(
                        self.stream.as_raw_fd(),
                        libc::POLLOUT,
                        left.as_millis().max(1) as i32,
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
fn poll_fd(fd: RawFd, events: i16, timeout_ms: i32) -> io::Result<()> {
    loop {
        let mut fd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if result > 0 {
            return Ok(());
        } // Read/write handles EOF/socket errors.
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "runtime socket timed out",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeVm {
        events: Vec<&'static str>,
        failure: Option<&'static str>,
    }
    impl FakeVm {
        fn event(&mut self, event: &'static str) -> Result<(), String> {
            self.events.push(event);
            if self.failure == Some(event) {
                Err(format!("injected {event} timeout"))
            } else {
                Ok(())
            }
        }
    }
    impl ControlledVm for FakeVm {
        fn pause_cpus(&mut self) -> Result<(), String> {
            self.event("pause_cpus")
        }
        fn resume_cpus(&mut self) -> Result<(), String> {
            self.event("resume_cpus")
        }
        fn pause_devices(&mut self) -> Result<(), String> {
            self.event("pause_devices")
        }
        fn resume_devices(&mut self) {
            self.event("resume_devices").unwrap();
        }
    }
    fn make_control() -> (RuntimeControl, UnixStream) {
        let (client, server) = UnixStream::pair().unwrap();
        let mut events = EventManager::new().unwrap();
        (RuntimeControl::new(server, &mut events).unwrap(), client)
    }
    fn request(command: &str) -> Value {
        json!({"request_id": 17, "command": command})
    }
    #[test]
    fn pause_resume_order_and_idempotence() {
        let (mut control, _client) = make_control();
        let mut vm = FakeVm::default();
        for _ in 0..2 {
            assert_eq!(
                control.execute(&request("pause"), &mut vm)["state"],
                "paused"
            );
        }
        assert!(!control.runs_devices());
        for _ in 0..2 {
            assert_eq!(
                control.execute(&request("resume"), &mut vm)["state"],
                "running"
            );
        }
        assert_eq!(
            vm.events,
            [
                "pause_cpus",
                "pause_devices",
                "resume_devices",
                "resume_cpus"
            ]
        );
        assert!(control.runs_devices());
    }
    #[test]
    fn failure_is_never_a_successful_pause_or_retry() {
        for failure in ["pause_cpus", "pause_devices", "resume_cpus"] {
            let (mut control, _client) = make_control();
            let mut vm = FakeVm {
                failure: Some(failure),
                ..Default::default()
            };
            let pause = control.execute(&request("pause"), &mut vm);
            let result = if failure == "resume_cpus" {
                control.execute(&request("resume"), &mut vm)
            } else {
                pause
            };
            assert_eq!(result["ok"], false);
            assert_eq!(result["state"], "faulted");
            assert!(!control.runs_devices());
            let count = vm.events.len();
            assert_eq!(control.execute(&request("resume"), &mut vm)["ok"], false);
            assert_eq!(
                control.execute(&request("status"), &mut vm)["state"],
                "faulted"
            );
            assert_eq!(vm.events.len(), count);
        }
    }
    #[test]
    fn partial_frames_and_request_ids_are_preserved() {
        let (mut control, mut client) = make_control();
        client.write_all(b"{\"request_id\":17,").unwrap();
        assert!(control.read_request().unwrap().is_none());
        client.write_all(b"\"command\":\"status\"}\n").unwrap();
        let request = control.read_request().unwrap().unwrap();
        let result = control.execute(&request, &mut FakeVm::default());
        assert_eq!(result["request_id"], 17);
        assert_eq!(result["ok"], true);
    }
    #[test]
    fn oversized_frames_and_disconnected_controller_fail() {
        let (mut control, mut client) = make_control();
        client
            .write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1])
            .unwrap();
        assert_eq!(
            control.read_request().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let (mut control, client) = make_control();
        drop(client);
        assert_eq!(
            control.read_request().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    #[test]
    fn controlled_vm_cannot_share_a_process_in_either_start_order() {
        use std::sync::atomic::AtomicUsize;
        let starts = AtomicUsize::new(0);
        register_vm_start(&starts, true).unwrap();
        assert!(register_vm_start(&starts, true).is_err());
        assert!(register_vm_start(&starts, false).is_err());
        let starts = AtomicUsize::new(0);
        register_vm_start(&starts, false).unwrap();
        register_vm_start(&starts, false).unwrap();
        assert!(register_vm_start(&starts, true).is_err());
    }

    #[test]
    fn descriptor_is_duplicated_without_stealing_ownership() {
        let (_client, server) = UnixStream::pair().unwrap();
        let duplicate = duplicate_stream(server.as_raw_fd()).unwrap();
        assert_ne!(duplicate.as_raw_fd(), server.as_raw_fd());
        drop(duplicate);
        server.peer_addr().unwrap();
        assert!(duplicate_stream(-1).is_err());
    }
}

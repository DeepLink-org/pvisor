//! The guest agent protocol: exec processes inside pVisor VMs.
//!
//! Transport: one vsock connection per exec. The shim (host) connects to a
//! unix socket that pvisor-vm proxies into the VM (`VmConfiguration::vsock_port`
//! with `listen = true`); the agent (guest) listens on the vsock port and
//! serves each connection with one exec'd process.
//!
//! Framing on every connection (little-endian):
//!
//! ```text
//!   [channel: u8][length: u32][payload: length bytes]
//! ```
//!
//! channel 0 carries JSON control messages, 1/2/3 carry stdin/stdout/stderr
//! bytes. A zero-length stdin frame signals EOF. Closing the connection
//! kills the exec'd process on the guest side.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// Well-known vsock port for the pVisor agent.
pub const AGENT_VSOCK_PORT: u32 = 0x7076; // "pv"

/// CLI argument that turns the shim binary into the guest agent.
pub const AGENT_ARG: &str = "--pvisor-shim-guest-agent";

/// Guest-side path the agent binary is copied to at VM boot.
pub const AGENT_GUEST_PATH: &str = "/.pvisor-agent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Control = 0,
    Stdin = 1,
    Stdout = 2,
    Stderr = 3,
}

impl Channel {
    fn from_u8(value: u8) -> Option<Channel> {
        match value {
            0 => Some(Channel::Control),
            1 => Some(Channel::Stdin),
            2 => Some(Channel::Stdout),
            3 => Some(Channel::Stderr),
            _ => None,
        }
    }
}

/// Control messages (channel 0, JSON payload).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// Host -> guest: run this process; reply `started`, then stream.
    ExecStart {
        argv: Vec<String>,
        env: Vec<String>,
        cwd: String,
    },
    /// Guest -> host: the process is running with this guest pid.
    Started { pid: u32 },
    /// Guest -> host: the process exited with this status.
    Exited { status: u32 },
    /// Either direction: something went wrong for this connection.
    Error { message: String },
}

/// Encode one frame into `out`.
pub fn encode_frame(out: &mut Vec<u8>, channel: Channel, payload: &[u8]) {
    out.push(channel as u8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

/// Encode one control message as a frame.
pub fn encode_control(out: &mut Vec<u8>, message: &Control) -> serde_json::Result<()> {
    let payload = serde_json::to_vec(message)?;
    encode_frame(out, Channel::Control, &payload);
    Ok(())
}

/// One decoded frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub channel: Channel,
    pub payload: Vec<u8>,
}

/// Blocking frame reader over any `Read`.
pub struct FrameReader<R: Read> {
    inner: R,
}

impl<R: Read> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        FrameReader { inner }
    }

    pub fn read_frame(&mut self) -> std::io::Result<Option<Frame>> {
        let mut header = [0u8; 5];
        match self.inner.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some(channel) = Channel::from_u8(header[0]) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid channel {}", header[0]),
            ));
        };
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; length];
        self.inner.read_exact(&mut payload)?;
        Ok(Some(Frame { channel, payload }))
    }

    pub fn read_control(&mut self) -> std::io::Result<Option<Control>> {
        loop {
            let Some(frame) = self.read_frame()? else {
                return Ok(None);
            };
            match frame.channel {
                Channel::Control => {
                    let message = serde_json::from_slice(&frame.payload).map_err(|error| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("invalid control message: {error}"),
                        )
                    })?;
                    return Ok(Some(message));
                }
                // Bytes before the first control message cannot be routed
                // anywhere yet; drop them.
                _ => continue,
            }
        }
    }
}

/// Blocking frame writer over any `Write`.
pub struct FrameWriter<W: Write> {
    inner: W,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(inner: W) -> Self {
        FrameWriter { inner }
    }

    pub fn write_frame(&mut self, channel: Channel, payload: &[u8]) -> std::io::Result<()> {
        let mut buffer = Vec::with_capacity(5 + payload.len());
        encode_frame(&mut buffer, channel, payload);
        self.inner.write_all(&buffer)?;
        self.inner.flush()
    }

    pub fn write_control(&mut self, message: &Control) -> std::io::Result<()> {
        let payload = serde_json::to_vec(message).map_err(|error| {
            std::io::Error::other(format!("serialize control message: {error}"))
        })?;
        self.write_frame(Channel::Control, &payload)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    /// Guest agent entry; Ok(false) when not invoked in agent mode,
    /// otherwise never returns.
    pub fn run_guest_agent_if_requested() -> anyhow::Result<bool> {
        let args: Vec<String> = std::env::args().collect();
        if !args.iter().any(|arg| arg == AGENT_ARG) {
            return Ok(false);
        }
        let code = match guest_agent_main() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("pvisor shim guest agent: {error:#}");
                1
            }
        };
        std::process::exit(code)
    }

    /// Listen on the vsock port and serve one exec per connection.
    fn guest_agent_main() -> anyhow::Result<()> {
        let listener = vsock_listen(AGENT_VSOCK_PORT)?;
        eprintln!("pvisor shim guest agent listening on vsock:{AGENT_VSOCK_PORT}");
        loop {
            let (stream, _) = listener.accept()?;
            std::thread::spawn(move || {
                if let Err(error) = serve_connection(stream) {
                    eprintln!("pvisor shim guest agent session: {error:#}");
                }
            });
        }
    }

    fn vsock_listen(port: u32) -> anyhow::Result<std::os::unix::net::UnixListener> {
        use std::os::fd::FromRawFd;
        use std::os::unix::net::UnixListener as VsockListener;
        // AF_VSOCK has the same socket API shape as AF_UNIX on Linux; use a
        // sockaddr_vm built by hand.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            anyhow::bail!("socket(AF_VSOCK): {}", std::io::Error::last_os_error());
        }
        let address = libc::sockaddr_vm {
            svm_family: libc::AF_VSOCK as u16,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: libc::VMADDR_CID_ANY,
            svm_zero: [0; 4],
        };
        let bind = unsafe {
            libc::bind(
                fd,
                std::ptr::from_ref(&address).cast(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if bind != 0 || unsafe { libc::listen(fd, 16) } != 0 {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            anyhow::bail!("vsock listen on {port}: {error}");
        }
        // SAFETY-ish: the fd is a listening AF_VSOCK socket; treating it as
        // a UnixListener keeps the I/O methods without touching path logic.
        // We never call path-based methods on it.
        let listener = unsafe { VsockListener::from_raw_fd(fd) };
        Ok(listener)
    }

    /// Serve one exec: read the start request, spawn, stream, report exit.
    pub(super) fn serve_connection(stream: std::os::unix::net::UnixStream) -> anyhow::Result<()> {
        let read_half = stream.try_clone()?;
        let mut writer = FrameWriter::new(stream);

        let Some(Control::ExecStart { argv, env, cwd }) =
            FrameReader::new(&read_half).read_control()?
        else {
            anyhow::bail!("expected exec start request");
        };
        if argv.is_empty() {
            writer.write_control(&Control::Error {
                message: "empty argv".to_string(),
            })?;
            return Ok(());
        }

        let mut child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().map(|entry| split_env(entry)))
            .current_dir(&cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let pid = child.id();
        writer.write_control(&Control::Started { pid })?;

        // Guest process output -> frames.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        writer
            .inner
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
        let writer = std::sync::Arc::new(std::sync::Mutex::new(writer));
        let writer_stdout = std::sync::Arc::clone(&writer);
        let writer_stderr = std::sync::Arc::clone(&writer);
        let stop_output = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_stdout = std::sync::Arc::clone(&stop_output);
        let stop_stderr = std::sync::Arc::clone(&stop_output);
        let out_handle = std::thread::spawn(move || {
            pump_to_frames(stdout, writer_stdout, Channel::Stdout, stop_stdout)
        });
        let err_handle = std::thread::spawn(move || {
            pump_to_frames(stderr, writer_stderr, Channel::Stderr, stop_stderr)
        });

        // Socket -> child stdin in a thread of its own: the main thread
        // blocks in `wait` instead, so a process that exits while the host
        // keeps the connection open still gets its Exited message.
        let mut reader = FrameReader::new(read_half);
        let mut child_stdin = child.stdin.take();
        let kill_target = pid as i32;
        let alive = std::sync::Arc::new(std::sync::Mutex::new(true));
        let stdin_alive = std::sync::Arc::clone(&alive);
        let stdin_handle = std::thread::spawn(move || {
            while let Ok(Some(frame)) = reader.read_frame() {
                if frame.channel != Channel::Stdin {
                    continue;
                }
                if frame.payload.is_empty() {
                    child_stdin.take();
                    continue;
                }
                let Some(stdin) = child_stdin.as_mut() else {
                    break;
                };
                if stdin.write_all(&frame.payload).is_err() {
                    break;
                }
            }
            // Socket EOF is the host-side kill for this exec.
            let live = stdin_alive.lock().unwrap();
            if *live {
                unsafe { libc::kill(kill_target, libc::SIGKILL) };
            }
        });

        // Observe exit without reaping so socket EOF cannot signal a recycled PID.
        let mut exit_info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut exit_info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        let mut live = alive.lock().unwrap();
        *live = false;
        let status = child
            .wait()
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(255) as u32;
        drop(live);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while (!out_handle.is_finished() || !err_handle.is_finished())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let incomplete = !out_handle.is_finished() || !err_handle.is_finished();
        stop_output.store(true, std::sync::atomic::Ordering::Release);
        let output_ok = out_handle.join().unwrap_or(false) & err_handle.join().unwrap_or(false);
        let status = if incomplete || !output_ok {
            255
        } else {
            status
        };
        let _ = writer
            .lock()
            .unwrap()
            .write_control(&Control::Exited { status });
        // Dropping our writer closes the guest side; the host then closes
        // its end, which unblocks and ends the stdin thread.
        let _ = writer
            .lock()
            .unwrap()
            .inner
            .shutdown(std::net::Shutdown::Read);
        drop(writer);
        let _ = stdin_handle.join();
        Ok(())
    }

    fn pump_to_frames<R: Read + std::os::fd::AsRawFd>(
        input: Option<R>,
        output: std::sync::Arc<std::sync::Mutex<FrameWriter<std::os::unix::net::UnixStream>>>,
        channel: Channel,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        let Some(mut input) = input else { return true };
        let mut buffer = [0u8; 8192];
        loop {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return false;
            }
            let mut fd = libc::pollfd {
                fd: input.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, 25) };
            if ready == 0 {
                continue;
            }
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
            match input.read(&mut buffer) {
                Ok(0) => return true,
                Err(_) => return false,
                Ok(n) => {
                    if output
                        .lock()
                        .unwrap()
                        .write_frame(channel, &buffer[..n])
                        .is_err()
                    {
                        return false;
                    }
                }
            }
        }
    }

    fn split_env(entry: &str) -> (String, String) {
        match entry.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (entry.to_string(), String::new()),
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::run_guest_agent_if_requested;

#[cfg(test)]
mod tests {
    use super::*;

    fn control_bytes(message: &Control) -> Vec<u8> {
        serde_json::to_vec(message).expect("serialize control")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guest_stdin_eof_drains_output_before_exit() {
        let (client, guest) = std::os::unix::net::UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || super::linux::serve_connection(guest));
        let mut writer = FrameWriter::new(client.try_clone().unwrap());
        writer
            .write_control(&Control::ExecStart {
                argv: vec!["/bin/cat".into()],
                env: Vec::new(),
                cwd: "/".into(),
            })
            .unwrap();
        let mut reader = FrameReader::new(client);
        assert!(matches!(
            reader.read_control().unwrap(),
            Some(Control::Started { .. })
        ));
        writer.write_frame(Channel::Stdin, b"tail").unwrap();
        writer.write_frame(Channel::Stdin, b"").unwrap();
        let mut output = Vec::new();
        loop {
            let frame = reader.read_frame().unwrap().unwrap();
            if frame.channel == Channel::Stdout {
                output.extend_from_slice(&frame.payload);
            }
            if frame.channel == Channel::Control {
                assert_eq!(
                    serde_json::from_slice::<Control>(&frame.payload).unwrap(),
                    Control::Exited { status: 0 }
                );
                break;
            }
        }
        assert_eq!(output, b"tail");
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn frames_round_trip_through_the_codec() {
        let mut buffer = Vec::new();
        encode_frame(
            &mut buffer,
            Channel::Control,
            &control_bytes(&Control::Started { pid: 4242 }),
        );
        encode_frame(&mut buffer, Channel::Stdin, b"hello");
        encode_frame(&mut buffer, Channel::Stdout, &[]);
        encode_frame(&mut buffer, Channel::Stderr, &[0xff, 0xfe]);

        let mut reader = FrameReader::new(&buffer[..]);
        let first = reader.read_frame().expect("first frame").expect("some");
        assert_eq!(first.channel, Channel::Control);
        let message: Control = serde_json::from_slice(&first.payload).expect("control");
        assert_eq!(message, Control::Started { pid: 4242 });

        let second = reader.read_frame().expect("second frame").expect("some");
        assert_eq!(
            (second.channel, second.payload.as_slice()),
            (Channel::Stdin, b"hello".as_slice())
        );

        let third = reader.read_frame().expect("third frame").expect("some");
        assert_eq!(third.channel, Channel::Stdout);
        assert!(third.payload.is_empty());

        let fourth = reader.read_frame().expect("fourth frame").expect("some");
        assert_eq!(fourth.channel, Channel::Stderr);
        assert_eq!(fourth.payload, vec![0xff, 0xfe]);

        assert!(reader.read_frame().expect("eof").is_none());
    }

    #[test]
    fn control_messages_survive_serde() {
        let messages = vec![
            Control::ExecStart {
                argv: vec!["/bin/ls".to_string(), "-l /tmp with space".to_string()],
                env: vec!["A=b c".to_string()],
                cwd: "/work dir".to_string(),
            },
            Control::Started { pid: 1 },
            Control::Exited { status: 137 },
            Control::Error {
                message: "nope".to_string(),
            },
        ];
        for message in &messages {
            let mut buffer = Vec::new();
            encode_control(&mut buffer, message).expect("encode");
            let mut reader = FrameReader::new(&buffer[..]);
            let decoded = reader.read_control().expect("read").expect("some");
            assert_eq!(&decoded, message);
        }
    }

    #[test]
    fn invalid_channel_is_rejected() {
        let mut buffer = Vec::new();
        buffer.push(9);
        buffer.extend_from_slice(&0u32.to_le_bytes());
        let mut reader = FrameReader::new(&buffer[..]);
        let error = reader.read_frame().expect_err("invalid channel");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}

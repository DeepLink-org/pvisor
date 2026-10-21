use crate::api::{
    CancelExec, ExecOutcome, ExecRelayError, ExecRequest, ExecRuntime, ExecStartError,
};
use pvisor_guest::control::{Reply, Request, StdinFrame, read_frame, write_frame};
use std::{
    fs::File,
    io::{self, Read, Write},
    net::Shutdown,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// An accepted exec owning its duplex connection. Drop cancels a live exec.
pub struct ExecSession {
    stream: UnixStream,
    pid: u32,
}
/// A duplicate socket used only for explicit cancellation.
pub struct ExecCancellation {
    stream: UnixStream,
}

impl CancelExec for ExecCancellation {
    fn cancel(&self) -> io::Result<()> {
        match self.stream.shutdown(Shutdown::Both) {
            Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
            result => result,
        }
    }
}
impl Drop for ExecSession {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl ExecRuntime for ExecSession {
    fn start(mut stream: UnixStream, request: ExecRequest) -> Result<Self, ExecStartError> {
        write_frame(&mut stream, &Request::Exec(request))?;
        let pid = match read_frame(&mut stream)? {
            Reply::Started { pid } if pid > 0 => pid,
            Reply::Error { message } => {
                let code = match read_frame(&mut stream)? {
                    Reply::Exit { code } => code,
                    _ => return Err(invalid("guest rejected exec without exit status").into()),
                };
                return Err(ExecStartError::Rejected { message, code });
            }
            _ => return Err(invalid("expected guest exec Started response").into()),
        };
        // Darwin may reject setsockopt once a fast exec has closed its end.
        // Buffered Output/Exit frames must still be consumed in that case.
        for result in [
            stream.set_read_timeout(None),
            stream.set_write_timeout(None),
        ] {
            if let Err(error) = result
                && !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTCONN))
            {
                return Err(error.into());
            }
        }
        Ok(Self { stream, pid })
    }
    fn pid(&self) -> u32 {
        self.pid
    }
    fn cancellation(&self) -> io::Result<ExecCancellation> {
        Ok(ExecCancellation {
            stream: self.stream.try_clone()?,
        })
    }
    fn relay(
        mut self,
        input: Option<File>,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<ExecOutcome, ExecRelayError> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker = if let Some(input) = input {
            let mut stream = self.stream.try_clone()?;
            let stop = stop.clone();
            Some(std::thread::spawn(move || {
                if send_input(input, &mut stream, &stop).is_err() {
                    // Signal cancellation without discarding a buffered Exit
                    // from a command that finished before it read stdin.
                    let _ = stream.shutdown(Shutdown::Write);
                }
            }))
        } else {
            if write_frame(&mut self.stream, &StdinFrame::Eof {}).is_err() {
                let _ = self.stream.shutdown(Shutdown::Write);
            }
            None
        };
        let result = (|| loop {
            match read_frame(&mut self.stream)? {
                Reply::Output { stream: 1, bytes } => {
                    stdout.write_all(&bytes).map_err(ExecRelayError::Output)?;
                    stdout.flush().map_err(ExecRelayError::Output)?;
                }
                Reply::Output { stream: 2, bytes } => {
                    stderr.write_all(&bytes).map_err(ExecRelayError::Output)?;
                    stderr.flush().map_err(ExecRelayError::Output)?;
                }
                Reply::ExecExit(outcome) => return Ok(outcome),
                Reply::Error { message } => return Err(io::Error::other(message).into()),
                _ => return Err(invalid("invalid guest exec output").into()),
            }
        })();
        stop.store(true, Ordering::Release);
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        result
    }
}

fn send_input(mut input: File, stream: &mut UnixStream, stop: &AtomicBool) -> io::Result<()> {
    let mut bytes = [0; 16384];
    while !stop.load(Ordering::Acquire) {
        let mut fd = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return write_frame(stream, &StdinFrame::Eof {});
        }
        if ready == 0 {
            continue;
        }
        if fd.revents & libc::POLLNVAL != 0 {
            return write_frame(stream, &StdinFrame::Eof {});
        }
        let size = match input.read(&mut bytes) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            // The shim's FIFO reader treats a local input failure as stdin EOF.
            // Keep output connected so a command may still report its status.
            Err(_) => 0,
            Ok(size) => size,
        };
        if size == 0 {
            return write_frame(stream, &StdinFrame::Eof {});
        }
        write_frame(
            stream,
            &StdinFrame::Data {
                bytes: bytes[..size].to_vec(),
            },
        )?;
    }
    Ok(())
}

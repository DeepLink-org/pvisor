//! Session exec frontend. The owner admits commands; the caller owns terminal IO.
#[cfg(target_os = "linux")]
use anyhow::{Context, ensure};
use std::path::Path;

pub(super) fn run(
    selector: &Path,
    argv: &[String],
    interactive: bool,
    tty: bool,
) -> anyhow::Result<i32> {
    #[cfg(target_os = "linux")]
    {
        linux(selector, argv, interactive, tty)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (selector, argv, interactive, tty);
        anyhow::bail!("CAPABILITY_UNSUPPORTED: Session exec is Linux Host only")
    }
}

#[cfg(target_os = "linux")]
fn linux(selector: &Path, argv: &[String], interactive: bool, tty: bool) -> anyhow::Result<i32> {
    use pvisor::host_transport::{InstanceCommand, read_host_frame_sync, write_host_frame_sync};
    use pvisor::{
        AGENTCTL_HOST_VERSION, AgentCtlHostRequest, AgentCtlHostResponse, AgentCtlTarget,
    };
    use std::os::fd::{AsFd, BorrowedFd};
    ensure!(!argv.is_empty(), "Session exec requires a command");
    ensure!(
        !tty || (interactive && unsafe { libc::isatty(0) } == 1),
        "Session exec -t requires -i and a terminal"
    );
    let record = super::runtime::selected(Some(selector), Path::new(".pvisor/capture"))?;
    super::host_service::check_record(&record)?;
    let execution = record
        .execution_id
        .as_deref()
        .context("Session has no Execution identity")?;
    let endpoint = record.live_control.as_ref().context(
        "Session exec requires a live owner; stopped/suspended Sessions are not resumed",
    )?;
    let mut stream = endpoint.connect()?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let request = AgentCtlHostRequest {
        version: AGENTCTL_HOST_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        target: Some(AgentCtlTarget {
            session_id: record.run_id.clone(),
            execution_id: Some(execution.into()),
            generation: None,
        }),
        command: InstanceCommand::Exec {
            argv: argv.to_vec(),
            tty,
        },
    };
    request.validate()?;
    let mut pty = tty.then(Pty::new).transpose()?;
    let null = std::fs::File::open("/dev/null")?;
    let stdio = if let Some(pty) = &pty {
        [pty.slave.as_ref().unwrap().as_fd(); 3]
    } else {
        [
            if interactive {
                unsafe { BorrowedFd::borrow_raw(0) }
            } else {
                null.as_fd()
            },
            unsafe { BorrowedFd::borrow_raw(1) },
            unsafe { BorrowedFd::borrow_raw(2) },
        ]
    };
    write_host_frame_sync(&mut stream, &request)?;
    pvisor::host_transport::send_exec_stdio(&stream, stdio)?;
    if let Some(pty) = &mut pty {
        pty.slave.take();
    }
    let started: AgentCtlHostResponse<serde_json::Value> = read_host_frame_sync(&mut stream)
        .context("Session exec startup reply unavailable; not retried")?;
    started.validate(&request.request_id)?;
    let started = started.result?;
    ensure!(
        started["started"] == true
            && started["session_id"].as_str() == Some(&record.run_id)
            && started["execution_id"].as_str() == Some(execution),
        "exec startup identity mismatch"
    );
    stream.set_read_timeout(None)?;
    if tty {
        eprintln!("pvisor exec: connected to {} ({execution})", record.run_id);
    }
    // No replay after this point. Closing this connection cancels only this
    // exec process range, not the owning Session or main workload.
    if let Some(pty) = &mut pty {
        pty.relay_until_reply(&stream)?;
    }
    let response: AgentCtlHostResponse<serde_json::Value> = read_host_frame_sync(&mut stream)
        .context("Session exec response unavailable; not retried")?;
    response.validate(&request.request_id)?;
    let value = response.result?;
    ensure!(
        value["session_id"].as_str() == Some(&record.run_id)
            && value["execution_id"].as_str() == Some(execution),
        "exec result identity mismatch"
    );
    let code = i32::try_from(
        value["exit_code"]
            .as_i64()
            .context("exec exit code unavailable")?,
    )?;
    if let Some(pty) = &mut pty {
        pty.drain()?;
    }
    Ok(code)
}

#[cfg(target_os = "linux")]
struct Pty {
    master: std::os::fd::OwnedFd,
    slave: Option<std::os::fd::OwnedFd>,
}

#[cfg(target_os = "linux")]
impl Pty {
    fn new() -> anyhow::Result<Self> {
        use std::os::fd::FromRawFd;
        let mut master = -1;
        let mut slave = -1;
        let size = Self::size();
        ensure!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            } == 0,
            "allocate Session exec PTY: {}",
            std::io::Error::last_os_error()
        );
        let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
        let slave = unsafe { std::os::fd::OwnedFd::from_raw_fd(slave) };
        use std::os::fd::AsRawFd;
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            ensure!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "mark exec PTY CLOEXEC"
            );
        }
        Ok(Self {
            master,
            slave: Some(slave),
        })
    }

    fn size() -> libc::winsize {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } != 0
            || size.ws_row == 0
            || size.ws_col == 0
        {
            size.ws_row = 24;
            size.ws_col = 80;
        }
        size
    }

    fn output(&self) -> anyhow::Result<bool> {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let mut bytes = [0u8; 8192];
        let n = unsafe {
            libc::read(
                self.master.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n == 0 || (n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EIO)) {
            return Ok(false);
        }
        if n < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        std::io::stdout().write_all(&bytes[..n as usize])?;
        std::io::stdout().flush()?;
        Ok(true)
    }

    fn relay_until_reply(&mut self, stream: &std::os::unix::net::UnixStream) -> anyhow::Result<()> {
        use std::os::fd::AsRawFd;
        let mut input = 0;
        let mut output = self.master.as_raw_fd();
        loop {
            let size = Self::size();
            unsafe {
                libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size);
            }
            let mut fds = [
                libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: input,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: output,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 3, 50) } < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(std::io::Error::last_os_error().into());
            }
            if fds[2].revents != 0 && !self.output()? {
                output = -1;
            }
            if fds[0].revents != 0 {
                return Ok(());
            }
            if fds[1].revents != 0 {
                let mut bytes = [0u8; 8192];
                let n = unsafe { libc::read(input, bytes.as_mut_ptr().cast(), bytes.len()) };
                if n <= 0 {
                    input = -1;
                    unsafe {
                        libc::write(self.master.as_raw_fd(), b"\x04".as_ptr().cast(), 1);
                    }
                } else {
                    let mut offset = 0;
                    while offset < n as usize {
                        let count = unsafe {
                            libc::write(
                                self.master.as_raw_fd(),
                                bytes[offset..n as usize].as_ptr().cast(),
                                n as usize - offset,
                            )
                        };
                        if count < 0 {
                            return Err(std::io::Error::last_os_error().into());
                        }
                        offset += count as usize;
                    }
                }
            }
        }
    }

    fn drain(&mut self) -> anyhow::Result<()> {
        use std::os::fd::AsRawFd;
        loop {
            let mut fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 || fd.revents == 0 || !self.output()? {
                return Ok(());
            }
        }
    }
}

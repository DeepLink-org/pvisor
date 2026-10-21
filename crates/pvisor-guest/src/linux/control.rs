//! Bounded vsock request adapter; process launch, signalling and reaping are shared.
use super::{
    check,
    process::{
        ProcessReader, ProcessTarget, ProcessWriter, Processes, Spawned, read_chunk, spawn_error,
    },
};
use pvisor_guest::{
    GuestConfig,
    control::{self, Reply, Request, StdinFrame, read_frame, write_frame},
};
use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

type Input = Arc<Mutex<Option<Box<dyn ProcessWriter>>>>;

pub(super) fn listener() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    check(fd)?;
    let listener = unsafe { OwnedFd::from_raw_fd(fd) };
    let address = libc::sockaddr_vm {
        svm_family: libc::AF_VSOCK as _,
        svm_reserved1: 0,
        svm_port: control::CONTROL_PORT,
        svm_cid: libc::VMADDR_CID_ANY,
        svm_zero: [0; 4],
    };
    check(unsafe {
        libc::bind(
            fd,
            (&address as *const libc::sockaddr_vm).cast(),
            std::mem::size_of_val(&address) as _,
        )
    })?;
    check(unsafe { libc::listen(fd, 32) })?;
    Ok(listener)
}

fn pump(
    mut reader: Box<dyn ProcessReader>,
    writer: Arc<Mutex<UnixStream>>,
    stream: u8,
    deadline: Arc<Mutex<Option<std::time::Instant>>>,
) -> io::Result<bool> {
    let mut buffer = [0; 16 * 1024];
    loop {
        if deadline
            .lock()
            .unwrap()
            .is_some_and(|end| std::time::Instant::now() >= end)
        {
            return Ok(false);
        }
        let mut fd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        let size = match read_chunk(&mut reader, &mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            result => result?,
        };
        if size == 0 {
            return Ok(true);
        }
        write_frame(
            &mut *writer.lock().unwrap(),
            &Reply::Output {
                stream,
                bytes: buffer[..size].to_vec(),
            },
        )?;
    }
}

// Exec input may fill a pipe while the command never reads it. Poll both the
// nonblocking pipe and the peer so cancellation still reaches the supervisor.
fn forward_exec_input(
    stream: &UnixStream,
    writer: &mut dyn ProcessWriter,
    mut bytes: &[u8],
) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
    check(flags)?;
    check(unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
    while !bytes.is_empty() {
        let mut fds = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLRDHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: writer.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[0].revents != 0 {
            return Err(io::Error::from(io::ErrorKind::ConnectionAborted));
        }
        if fds[1].revents == 0 {
            continue;
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(size) => bytes = &bytes[size..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn consume_input(stream: &mut UnixStream, output: &mut impl Write) -> io::Result<()> {
    loop {
        match read_frame::<StdinFrame>(stream)? {
            StdinFrame::Data { bytes } => {
                output.write_all(&bytes)?;
                output.flush()?;
            }
            StdinFrame::Eof {} => return Ok(()),
        }
    }
}

fn exec_config(base: &GuestConfig, request: control::ExecRequest) -> GuestConfig {
    let mut config = base.clone();
    config.argv = request.argv;
    if request.replace_env {
        config.env.clear();
    }
    for key in request.unset_env {
        config.env.remove(&key);
    }
    config.env.extend(request.env);
    if let Some(cwd) = request.cwd {
        config.cwd = cwd;
    }
    if let Some(limits) = request.limits {
        config.limits = limits;
    }
    let options = config.process.get_or_insert_default();
    if let Some(user) = request.user {
        options.user = Some(user);
    }
    options.tty = request.tty;
    options.stdin = pvisor_guest::StdinMode::Pipe;
    options.create_workdir = false;
    config
}

fn serve(
    mut stream: UnixStream,
    config: &GuestConfig,
    processes: &Processes,
    main: ProcessTarget,
    input: &Input,
) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let request: Request = read_frame(&mut stream)?;
    stream.set_read_timeout(None)?;
    match request {
        Request::Ping {} => write_frame(&mut stream, &Reply::Ready),
        Request::Input {} => {
            let mut input = input
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::other("stdin is closed or already attached"))?;
            write_frame(&mut stream, &Reply::Ready)?;
            consume_input(&mut stream, &mut input)?;
            drop(input);
            write_frame(&mut stream, &Reply::Ready)
        }
        Request::Stop { signal } => {
            processes.signal(&main, signal)?;
            write_frame(&mut stream, &Reply::Ready)
        }
        Request::Connect { port } => {
            let mut tcp = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_secs(10),
            )?;
            write_frame(&mut stream, &Reply::Ready)?;
            let mut input = stream.try_clone()?;
            let mut output = tcp.try_clone()?;
            std::thread::spawn(move || {
                let _ = consume_input(&mut input, &mut output);
                let _ = output.shutdown(std::net::Shutdown::Write);
            });
            let mut bytes = [0; 16384];
            loop {
                let size = tcp.read(&mut bytes)?;
                if size == 0 {
                    break;
                }
                write_frame(
                    &mut stream,
                    &Reply::Output {
                        stream: 1,
                        bytes: bytes[..size].to_vec(),
                    },
                )?;
            }
            write_frame(&mut stream, &Reply::Exit { code: 0 })
        }
        Request::Exec(request) => {
            let timeout = request.output_timeout_ms;
            if timeout.is_some_and(|ms| !(1..=60000).contains(&ms)) {
                write_frame(
                    &mut stream,
                    &Reply::Error {
                        message: "invalid output timeout".into(),
                    },
                )?;
                return write_frame(&mut stream, &Reply::Exit { code: 125 });
            }
            let timeout = timeout.map(std::time::Duration::from_millis);
            let config = exec_config(config, request);
            let Spawned {
                process,
                input,
                outputs,
            } = match processes.spawn(&config, true) {
                Ok(child) => child,
                Err(error) => {
                    let code = spawn_error(&error).unwrap_or(125);
                    write_frame(
                        &mut stream,
                        &Reply::Error {
                            message: error.to_string(),
                        },
                    )?;
                    return write_frame(&mut stream, &Reply::Exit { code });
                }
            };
            let exec_target = process.target();
            let setup = (|| {
                stream.set_write_timeout(timeout)?;
                let input = stream.try_clone()?;
                let output = Arc::new(Mutex::new(stream.try_clone()?));
                write_frame(&mut stream, &Reply::Started { pid: process.id() })?;
                Ok::<_, io::Error>((input, output))
            })();
            let (mut input_stream, output) = match setup {
                Ok(sockets) => sockets,
                Err(error) => {
                    let _ = processes.signal(&exec_target, libc::SIGKILL);
                    return Err(error);
                }
            };
            let mut stdin = input;
            let processes = processes.clone();
            let input_pump = std::thread::spawn(move || {
                loop {
                    match read_frame::<StdinFrame>(&mut input_stream) {
                        Ok(StdinFrame::Eof {}) => {
                            stdin.take();
                        }
                        Ok(StdinFrame::Data { bytes }) => {
                            if let Some(writer) = &mut stdin
                                && forward_exec_input(&input_stream, &mut **writer, &bytes).is_err()
                            {
                                let _ = processes.signal(&exec_target, libc::SIGKILL);
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = processes.signal(&exec_target, libc::SIGKILL);
                            break;
                        }
                    }
                }
            });
            let deadline = Arc::new(Mutex::new(None));
            let pumps: Vec<_> = outputs
                .into_iter()
                .map(|(reader, channel)| {
                    let writer = output.clone();
                    let deadline = deadline.clone();
                    std::thread::spawn(move || pump(reader, writer, channel, deadline))
                })
                .collect();
            let outcome = process.wait();
            *deadline.lock().unwrap() = timeout.map(|timeout| std::time::Instant::now() + timeout);
            let mut output_complete = true;
            for pump in pumps {
                output_complete &= matches!(pump.join(), Ok(Ok(true)));
            }
            let result = outcome.and_then(|mut outcome| {
                outcome.output_complete = output_complete;
                write_frame(&mut *output.lock().unwrap(), &Reply::ExecExit(outcome))
            });
            let _ = stream.shutdown(std::net::Shutdown::Read);
            let _ = input_pump.join();
            result
        }
    }
}

pub(super) fn start(
    listener: OwnedFd,
    config: GuestConfig,
    processes: Processes,
    main: ProcessTarget,
    input: Option<Box<dyn ProcessWriter>>,
) {
    let input: Input = Arc::new(Mutex::new(input));
    std::thread::spawn(move || {
        let active = Arc::new(AtomicUsize::new(0));
        loop {
            // UnixListener::accept rejects AF_VSOCK peer addresses.
            let fd = unsafe {
                libc::accept4(
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if fd < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            let stream = unsafe { UnixStream::from_raw_fd(fd) };
            let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of_val(&address) as libc::socklen_t;
            if unsafe {
                libc::getpeername(
                    stream.as_raw_fd(),
                    (&mut address as *mut libc::sockaddr_vm).cast(),
                    &mut size,
                )
            } != 0
                || address.svm_cid != libc::VMADDR_CID_HOST
            {
                continue;
            }
            let previous = active.fetch_add(1, Ordering::AcqRel);
            if !config.control_unlimited && previous >= 32 {
                active.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let active = active.clone();
            let config = config.clone();
            let processes = processes.clone();
            let input = input.clone();
            let main = main.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                if let Err(error) = serve(
                    stream.try_clone().unwrap(),
                    &config,
                    &processes,
                    main,
                    &input,
                ) {
                    let _ = write_frame(
                        &mut stream,
                        &Reply::Error {
                            message: error.to_string(),
                        },
                    );
                }
                active.fetch_sub(1, Ordering::AcqRel);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_guest::control::ExecRequest;
    use std::{collections::BTreeMap, time::Duration};

    fn base() -> GuestConfig {
        GuestConfig {
            argv: vec!["/bin/sleep".into(), "30".into()],
            env: BTreeMap::from([("INIT_ONLY".into(), "yes".into())]),
            cwd: "/".into(),
            limits: BTreeMap::from([("RLIMIT_NOFILE".into(), (64, 64))]),
            ..Default::default()
        }
    }

    #[test]
    fn exec_adapters_preserve_environment_and_limit_semantics() {
        let base = base();
        let inherited = exec_config(
            &base,
            ExecRequest {
                argv: vec!["cat".into()],
                ..Default::default()
            },
        );
        assert_eq!(inherited.env, base.env);
        assert_eq!(inherited.limits, base.limits);
        let replaced = exec_config(
            &base,
            ExecRequest {
                argv: vec!["cat".into()],
                replace_env: true,
                env: BTreeMap::from([("EXEC_ONLY".into(), "yes".into())]),
                limits: Some(BTreeMap::new()),
                ..Default::default()
            },
        );
        assert!(!replaced.env.contains_key("INIT_ONLY"));
        assert!(replaced.limits.is_empty());
        assert!(!replaced.process.unwrap().new_session);
    }

    fn exercise(request: ExecRequest, disconnected: bool, action: impl FnOnce(&mut UnixStream)) {
        let config = base();
        let processes = Processes::default();
        let main = processes.spawn(&config, false).unwrap().process;
        let main_target = main.target();
        let main_control = main_target.clone();
        let supervisor = processes.clone();
        let reaper = std::thread::spawn(move || supervisor.supervise(main));
        let (mut client, stream) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let controller = processes.clone();
        let (done, received) = std::sync::mpsc::channel();
        let service = std::thread::spawn(move || {
            let result = serve(
                stream,
                &config,
                &controller,
                main_control,
                &Arc::new(Mutex::new(None)),
            );
            done.send(result).unwrap();
        });
        write_frame(&mut client, &Request::Exec(request)).unwrap();
        assert!(
            matches!(read_frame::<Reply>(&mut client).unwrap(), Reply::Started { pid } if pid > 0)
        );
        action(&mut client);
        let finished = received.recv_timeout(Duration::from_secs(5));
        processes.signal(&main_target, libc::SIGTERM).unwrap();
        assert_eq!(reaper.join().unwrap().unwrap(), 143);
        let result = finished.expect("control handler leaked after exec");
        if !disconnected {
            result.unwrap();
        }
        service.join().unwrap();
    }

    #[test]
    fn shared_exec_drains_output_after_eof_and_applies_oci_limits() {
        exercise(ExecRequest {
            argv: vec!["/bin/sh".into(), "-c".into(),
                "test -z \"${INIT_ONLY+x}\" || exit 40; test \"$(ulimit -n)\" = 32 || exit 41; cat; echo stderr >&2; exit 23".into()],
            replace_env: true, limits: Some(BTreeMap::from([("RLIMIT_NOFILE".into(), (32,32))])),
            ..Default::default()
        }, false, |stream| {
            write_frame(stream, &StdinFrame::Data { bytes: b"tail".to_vec() }).unwrap();
            write_frame(stream, &StdinFrame::Eof {}).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            loop { match read_frame::<Reply>(stream).unwrap() {
                Reply::Output {stream:1, bytes} => out.extend(bytes),
                Reply::Output {stream:2, bytes} => err.extend(bytes),
                Reply::ExecExit(outcome) => { assert_eq!(outcome.code, 23); assert!(outcome.output_complete); break; }
                reply => panic!("unexpected {reply:?}"),
            }}
            assert_eq!(out, b"tail"); assert_eq!(err, b"stderr\n");
        });
    }

    fn disconnect_after(frame: StdinFrame) {
        exercise(
            ExecRequest {
                argv: vec!["/bin/sleep".into(), "30".into()],
                ..Default::default()
            },
            true,
            |stream| {
                write_frame(stream, &frame).unwrap();
                stream.shutdown(std::net::Shutdown::Both).unwrap();
            },
        );
    }

    #[test]
    fn disconnect_after_stdin_eof_cancels_the_live_exec() {
        disconnect_after(StdinFrame::Eof {});
    }

    #[test]
    fn disconnect_cancels_exec_even_when_its_stdin_pipe_is_full() {
        disconnect_after(StdinFrame::Data {
            bytes: vec![b'x'; 128 * 1024],
        });
    }

    #[test]
    fn inherited_output_descriptor_does_not_hold_exit_forever() {
        let (reader, mut inherited) = UnixStream::pair().unwrap();
        let (mut client, output) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let deadline = Arc::new(Mutex::new(None));
        let worker_deadline = deadline.clone();
        let (done, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            done.send(pump(
                Box::new(reader),
                Arc::new(Mutex::new(output)),
                1,
                worker_deadline,
            ))
            .unwrap();
        });
        inherited.write_all(b"tail").unwrap();
        assert!(
            matches!(read_frame::<Reply>(&mut client).unwrap(), Reply::Output {bytes, ..} if bytes == b"tail")
        );
        *deadline.lock().unwrap() = Some(std::time::Instant::now());
        assert!(
            !received
                .recv_timeout(Duration::from_secs(1))
                .expect("output pump leaked")
                .unwrap(),
            "truncated output must not report completion"
        );
        worker.join().unwrap();
        drop(inherited);
    }
    #[test]
    fn exec_inherits_session_identity_capabilities_cwd_and_env_policy() {
        let mut base = base();
        base.cwd = "/tmp".into();
        base.process = Some(pvisor_guest::ProcessConfig {
            new_session: true,
            user: Some("123:456".into()),
            capabilities: Some(vec![0, 1]),
            ..Default::default()
        });
        let config = exec_config(
            &base,
            ExecRequest {
                unset_env: vec!["INIT_ONLY".into()],
                ..Default::default()
            },
        );
        assert!(config.env.is_empty());
        assert_eq!(config.cwd, base.cwd);
        let options = config.process.unwrap();
        assert!(options.new_session);
        assert_eq!(options.user.as_deref(), Some("123:456"));
        assert_eq!(options.capabilities, Some(vec![0, 1]));
        let config = exec_config(
            &base,
            ExecRequest {
                cwd: Some("/".into()),
                user: Some("0:0".into()),
                ..Default::default()
            },
        );
        assert_eq!(config.cwd, std::path::PathBuf::from("/"));
        assert_eq!(config.process.unwrap().user.as_deref(), Some("0:0"));
    }

    #[test]
    fn signal_termination_is_distinct_from_an_explicit_high_exit_code() {
        for (script, signal) in [("exit 143", None), ("kill -TERM $$", Some(15))] {
            exercise(
                ExecRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
                    ..Default::default()
                },
                false,
                |stream| {
                    write_frame(stream, &StdinFrame::Eof {}).unwrap();
                    let Reply::ExecExit(outcome) = read_frame(stream).unwrap() else {
                        panic!("missing exit");
                    };
                    assert_eq!(outcome.code, 143);
                    assert_eq!(outcome.signal, signal);
                    assert!(outcome.output_complete);
                },
            );
        }
    }

    #[test]
    fn inherited_descendant_output_uses_the_callers_drain_policy() {
        for (timeout, complete) in [(Some(100), false), (None, true)] {
            exercise(
                ExecRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), "sleep 1 & exit 0".into()],
                    output_timeout_ms: timeout,
                    ..Default::default()
                },
                false,
                |stream| {
                    write_frame(stream, &StdinFrame::Eof {}).unwrap();
                    let Reply::ExecExit(outcome) = read_frame(stream).unwrap() else {
                        panic!("missing exit");
                    };
                    assert_eq!(outcome.code, 0);
                    assert_eq!(outcome.output_complete, complete);
                },
            );
        }
    }
}

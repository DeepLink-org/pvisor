//! Bounded vsock request adapter; process launch, signalling and reaping are shared.
use super::{
    check,
    process::{Processes, Spawned, read_chunk, spawn_error},
};
use pvisor_guest::{
    GuestConfig,
    control::{self, Reply, Request, StdinFrame, read_frame, write_frame},
};
use std::{
    collections::BTreeMap,
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

type Input = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

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

fn pump(mut reader: impl Read, writer: Arc<Mutex<UnixStream>>, stream: u8) -> io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    loop {
        let size = read_chunk(&mut reader, &mut buffer)?;
        if size == 0 {
            return Ok(());
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

fn serve(
    mut stream: UnixStream,
    config: &GuestConfig,
    processes: &Processes,
    pid: u32,
    input: &Input,
) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let request: Request = read_frame(&mut stream)?;
    let tracked = matches!(request, Request::Start { .. });
    let mut replace_env = false;
    let mut exec_limits = BTreeMap::new();
    let request = if let Request::Start { launch } = request {
        replace_env = launch.replace_env;
        exec_limits = launch.limits;
        Request::Exec {
            argv: launch.argv,
            env: launch.env,
            unset_env: Vec::new(),
            cwd: launch.cwd,
            user: launch.user,
            tty: launch.tty,
        }
    } else {
        request
    };
    stream.set_read_timeout(None)?;
    match request {
        Request::Ping {} => write_frame(&mut stream, &Reply::Ready),
        Request::Info {} => write_frame(
            &mut stream,
            &Reply::Value {
                value: super::files::info(config)?,
            },
        ),
        Request::File { request } => super::files::serve(&mut stream, request),
        Request::ReclaimAllocator {
            pid,
            generation,
            trim,
        } => write_frame(
            &mut stream,
            &Reply::Value {
                value: processes.reclaim_allocator(pid, generation, trim)?,
            },
        ),
        Request::Signal {
            pid,
            generation,
            signal,
        } => {
            processes.signal_tracked(pid, generation, signal)?;
            write_frame(&mut stream, &Reply::Ready)
        }
        Request::Resize {
            pid,
            generation,
            rows,
            columns,
        } => {
            processes.resize(pid, generation, rows, columns)?;
            write_frame(&mut stream, &Reply::Ready)
        }
        Request::Start { .. } => unreachable!("normalized above"),
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
            processes.signal(pid, signal)?;
            write_frame(&mut stream, &Reply::Ready)
        }
        Request::Connect { port } => {
            let tcp = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_secs(10),
            )?;
            forward_tcp(stream, tcp)
        }
        Request::Exec {
            argv,
            env,
            unset_env,
            cwd,
            user,
            tty,
        } => {
            let mut config = config.clone();
            // Transport/root construction belongs to PID 1's initial boot, not
            // to a subsequent process launch. Exec receives its own explicit
            // stdin/PTY ownership rather than inheriting the main attachment.
            config.serial_control = false;
            config.root_disk = None;
            config.argv = argv;
            if replace_env {
                config.env = env;
            } else {
                for key in unset_env {
                    config.env.remove(&key);
                }
                config.env.extend(env);
            }
            if tracked {
                config
                    .env
                    .retain(|key, _| !key.starts_with("PVISOR_NATIVE_"));
            }
            // OCI exec specs carry the process's complete rlimit set; an
            // empty override keeps the launch limits, the OCI default.
            if !exec_limits.is_empty() {
                config.limits = exec_limits;
            }
            if let Some(cwd) = cwd {
                config.cwd = cwd;
            }
            let options = config.process.get_or_insert_default();
            if let Some(user) = user {
                options.user = Some(user);
            }
            options.tty = tty;
            if tracked {
                options.new_session = true;
            }
            options.stdin = pvisor_guest::StdinMode::Pipe;
            options.create_workdir = false;
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
            let exec_pid = process.id();
            let exec_generation = process.generation();
            if let Err(error) = write_frame(
                &mut stream,
                &if tracked {
                    Reply::Started {
                        pid: exec_pid,
                        generation: exec_generation,
                    }
                } else {
                    Reply::Ready
                },
            ) {
                let _ = processes.signal_tracked(exec_pid, exec_generation, libc::SIGKILL);
                return Err(error);
            }
            let mut stdin = input.unwrap();
            let mut input = stream.try_clone()?;
            let exec_pid = process.id();
            let processes = processes.clone();
            std::thread::spawn(move || {
                if consume_input(&mut input, &mut stdin).is_err() {
                    let _ = processes.signal_tracked(exec_pid, exec_generation, libc::SIGKILL);
                }
            });
            let output = Arc::new(Mutex::new(stream.try_clone()?));
            let pumps: Vec<_> = outputs
                .into_iter()
                .map(|(reader, channel)| {
                    let writer = output.clone();
                    std::thread::spawn(move || pump(reader, writer, channel))
                })
                .collect();
            let code = process.wait()?;
            // An output pump that failed before EOF lost stream data; the
            // host must not read the following Exit as a clean result.
            let mut drained = true;
            for pump in pumps {
                if pump.join().is_ok_and(|result| result.is_err()) {
                    drained = false;
                }
            }
            if !drained {
                let _ = write_frame(
                    &mut *output.lock().unwrap(),
                    &Reply::Failure {
                        errno: libc::EIO,
                        message: "exec output could not be delivered".to_string(),
                    },
                );
            }
            write_frame(&mut *output.lock().unwrap(), &Reply::Exit { code })?;
            Ok(())
        }
    }
}

fn forward_tcp(mut stream: UnixStream, mut tcp: std::net::TcpStream) -> io::Result<()> {
    write_frame(&mut stream, &Reply::Ready)?;
    let mut input = stream.try_clone()?;
    let mut output = tcp.try_clone()?;
    let upstream = std::thread::spawn(move || {
        if consume_input(&mut input, &mut output).is_ok() {
            // Explicit EOF is a half-close: the TCP peer may still reply.
            let _ = output.shutdown(std::net::Shutdown::Write);
        } else {
            let _ = output.shutdown(std::net::Shutdown::Both);
            let _ = input.shutdown(std::net::Shutdown::Both);
        }
    });
    let result = (|| {
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
    })();
    // A completed response or error owns both pumps. Cancel blocked input reads
    // and TCP writes before joining; cloned descriptors must not retain a tunnel.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = tcp.shutdown(std::net::Shutdown::Both);
    let _ = upstream.join();
    result
}

pub(super) fn start(
    listener: OwnedFd,
    config: GuestConfig,
    processes: Processes,
    pid: u32,
    input: Option<Box<dyn Write + Send>>,
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
            if active.fetch_add(1, Ordering::AcqRel) >= 32 {
                active.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let active = active.clone();
            let config = config.clone();
            let processes = processes.clone();
            let input = input.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                if let Err(error) = serve(
                    stream.try_clone().unwrap(),
                    &config,
                    &processes,
                    pid,
                    &input,
                ) {
                    let _ = write_frame(
                        &mut stream,
                        &Reply::Failure {
                            errno: error.raw_os_error().unwrap_or(libc::EIO),
                            message: error.to_string(),
                        },
                    );
                }
                active.fetch_sub(1, Ordering::AcqRel);
            });
        }
    });
}

pub(super) fn serial_ports() -> io::Result<(std::fs::File, std::fs::File)> {
    use pvisor_guest::control::{SERIAL_COMMAND_PORT, SERIAL_SIGNAL_PORT};
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut paths = BTreeMap::new();
    while paths.len() != 2 {
        for entry in fs::read_dir("/sys/class/virtio-ports")? {
            let entry = entry?;
            if let Ok(name) = fs::read_to_string(entry.path().join("name"))
                && [SERIAL_COMMAND_PORT, SERIAL_SIGNAL_PORT].contains(&name.trim())
            {
                paths.insert(
                    name.trim().to_owned(),
                    Path::new("/dev").join(entry.file_name()),
                );
            }
        }
        if paths.len() == 2 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "serial control ports did not appear",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let open = |name| -> io::Result<std::fs::File> {
        let path = paths.get(name).unwrap();
        let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
        // Preserve PID 1's descriptor, but never give workload root a path to
        // steal host requests or forge replies. The launch contract also drops
        // mknod/mount/ptrace/module capabilities and sysfs is remounted read-only.
        super::mount_with_options("/dev/null", path, "", libc::MS_BIND, None)?;
        Ok(file)
    };
    let command = open(SERIAL_COMMAND_PORT)?;
    let signal = open(SERIAL_SIGNAL_PORT)?;
    for path in ["/sys/fs/cgroup", "/sys"] {
        super::mount_with_options(
            "none",
            Path::new(path),
            "",
            libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            None,
        )?;
    }
    Ok((command, signal))
}

/// Two independent, bounded multiplexed adapters. Explicit stdin EOF lets an
/// exec continue; unexpected attachment loss cancels its process group.
/// Signals never wait for the command port, and all launches use Processes.
pub(super) fn start_serial(
    mut ports: (std::fs::File, std::fs::File),
    config: GuestConfig,
    processes: Processes,
    pid: u32,
    input: Option<Box<dyn Write + Send>>,
) -> io::Result<()> {
    // This frame is emitted after process creation and before PID 1 can reap a
    // fast workload. It is distinct from a later Ping and cannot race its exit.
    write_frame(
        &mut ports.0,
        &pvisor_guest::control::SerialReply {
            id: 0,
            reply: Reply::Ready,
        },
    )?;
    let input = Arc::new(Mutex::new(input));
    for (port, signals) in [(ports.0, false), (ports.1, true)] {
        let config = config.clone();
        let processes = processes.clone();
        let input = input.clone();
        std::thread::spawn(move || {
            let result = port.try_clone().and_then(|writer| {
                serial_loop(port, writer, signals, config, processes, pid, input)
            });
            if let Err(error) = result {
                eprintln!("serial control ended: {error}");
            }
        });
    }
    Ok(())
}

struct SerialInputRoute {
    sender: std::sync::mpsc::SyncSender<StdinFrame>,
    cancel: UnixStream,
    eof: bool,
}

fn serial_loop(
    mut serial: impl Read,
    writer: impl Write + Send + 'static,
    signals: bool,
    config: GuestConfig,
    processes: Processes,
    pid: u32,
    input: Input,
) -> io::Result<()> {
    use pvisor_guest::control::{SerialHostFrame, SerialInputAck, SerialReply, SerialRequest};
    let trace = config
        .env
        .get("PVISOR_SERIAL_TRACE")
        .is_some_and(|v| v == "1");
    let writer = Arc::new(Mutex::new(writer));
    let active = Arc::new(AtomicUsize::new(0));
    let routes = Arc::new(Mutex::new(BTreeMap::<u64, SerialInputRoute>::new()));
    struct CloseRoutes(Arc<Mutex<BTreeMap<u64, SerialInputRoute>>>);
    impl Drop for CloseRoutes {
        fn drop(&mut self) {
            for (_, route) in std::mem::take(&mut *self.0.lock().unwrap()) {
                let _ = route.cancel.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    let _close = CloseRoutes(routes.clone());
    let mut last = 0;
    loop {
        if trace {
            eprintln!("serial control signal={signals}: awaiting request");
        }
        let SerialRequest { id, request } = match read_frame::<SerialHostFrame>(&mut serial)? {
            SerialHostFrame::Request(request) => request,
            SerialHostFrame::Cancel(cancel) => {
                if let Some(route) = routes.lock().unwrap().get(&cancel.cancel_id) {
                    let _ = route.cancel.shutdown(std::net::Shutdown::Both);
                }
                continue;
            }
            SerialHostFrame::Input(frame) => {
                let mut routes = routes.lock().unwrap();
                if let Some(route) = routes.get_mut(&frame.input_id) {
                    let oversized =
                        matches!(&frame.input, StdinFrame::Data { bytes } if bytes.len() > 16384);
                    let eof = matches!(frame.input, StdinFrame::Eof {});
                    if route.eof || oversized || route.sender.try_send(frame.input).is_err() {
                        let _ = route.cancel.shutdown(std::net::Shutdown::Both);
                        write_frame(
                            &mut *writer.lock().unwrap(),
                            &SerialReply {
                                id: frame.input_id,
                                reply: Reply::Failure {
                                    errno: libc::EBUSY,
                                    message: "stdin credit exceeded or input already closed".into(),
                                },
                            },
                        )?;
                    } else {
                        route.eof = eof;
                    }
                } else {
                    write_frame(
                        &mut *writer.lock().unwrap(),
                        &SerialInputAck {
                            input_ack: frame.input_id,
                        },
                    )?;
                }
                continue;
            }
        };
        if id == 0 || id <= last {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "serial request identity is stale",
            ));
        }
        last = id;
        if trace {
            eprintln!("serial control signal={signals}: received request");
        }
        let exec = matches!(request, Request::Exec { .. } | Request::Start { .. });
        let main_input = matches!(request, Request::Input {});
        let allowed = matches!(request, Request::Ping {})
            || (signals
                && matches!(
                    request,
                    Request::Stop { .. } | Request::Signal { .. } | Request::Resize { .. }
                ))
            || (!signals && (exec || main_input));
        if !allowed || active.load(Ordering::Acquire) >= control::SERIAL_MAX_REQUESTS {
            write_frame(
                &mut *writer.lock().unwrap(),
                &SerialReply {
                    id,
                    reply: Reply::Failure {
                        errno: if allowed { libc::EBUSY } else { libc::ENOTSUP },
                        message: if allowed {
                            "too many concurrent serial requests"
                        } else {
                            "unsupported serial operation"
                        }
                        .into(),
                    },
                },
            )?;
            continue;
        }
        let (mut client, mut server) = UnixStream::pair()?;
        write_frame(&mut client, &request)?;
        let input_worker = if exec || main_input {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            routes.lock().unwrap().insert(
                id,
                SerialInputRoute {
                    sender,
                    cancel: client.try_clone()?,
                    eof: false,
                },
            );
            let writer = writer.clone();
            let mut input_stream = client.try_clone()?;
            Some(std::thread::spawn(move || {
                while let Ok(frame) = receiver.recv() {
                    let eof = matches!(frame, StdinFrame::Eof {});
                    if write_frame(&mut input_stream, &frame).is_err() {
                        break;
                    }
                    if write_frame(
                        &mut *writer.lock().unwrap(),
                        &SerialInputAck { input_ack: id },
                    )
                    .is_err()
                    {
                        break;
                    }
                    if eof {
                        break;
                    }
                }
            }))
        } else {
            None
        };
        let config = config.clone();
        let processes = processes.clone();
        let input = input.clone();
        let routes = routes.clone();
        let writer = writer.clone();
        let active = active.clone();
        active.fetch_add(1, Ordering::AcqRel);
        std::thread::spawn(move || {
            struct Admission(Arc<AtomicUsize>);
            impl Drop for Admission {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _admission = Admission(active);
            let handler = std::thread::spawn(move || {
                let result = server
                    .try_clone()
                    .and_then(|stream| serve(stream, &config, &processes, pid, &input));
                if let Err(error) = result {
                    let _ = write_frame(
                        &mut server,
                        &Reply::Failure {
                            errno: error.raw_os_error().unwrap_or(libc::EIO),
                            message: error.to_string(),
                        },
                    );
                }
            });
            let result = (|| -> io::Result<()> {
                let mut input_ready = false;
                loop {
                    let reply: Reply = read_frame(&mut client)?;
                    let complete = matches!(reply, Reply::Failure { .. } | Reply::Exit { .. })
                        || (!exec
                            && matches!(reply, Reply::Ready)
                            && (!main_input || std::mem::replace(&mut input_ready, true)));
                    write_frame(&mut *writer.lock().unwrap(), &SerialReply { id, reply })?;
                    if complete {
                        break;
                    }
                }
                Ok(())
            })();
            let _ = client.shutdown(std::net::Shutdown::Both);
            routes.lock().unwrap().remove(&id);
            if let Err(error) = result {
                let _ = write_frame(
                    &mut *writer.lock().unwrap(),
                    &SerialReply {
                        id,
                        reply: Reply::Failure {
                            errno: libc::EIO,
                            message: error.to_string(),
                        },
                    },
                );
            }
            let _ = handler.join();
            if let Some(worker) = input_worker {
                let _ = worker.join();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn serial_request_id_replay_closes_the_connection_before_readmission() {
        use pvisor_guest::control::{SerialReply, SerialRequest};
        let (mut host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            serial_loop(
                guest.try_clone().unwrap(),
                guest,
                false,
                GuestConfig::default(),
                Processes::default(),
                0,
                Arc::new(Mutex::new(None)),
            )
        });
        write_frame(
            &mut host,
            &SerialRequest {
                id: 1,
                request: Request::Ping {},
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialReply>(&mut host).unwrap(),
            SerialReply {
                id: 1,
                reply: Reply::Ready
            }
        ));
        write_frame(
            &mut host,
            &SerialRequest {
                id: 1,
                request: Request::Ping {},
            },
        )
        .unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn serial_adapter_reuses_the_process_registry_and_keeps_signals_independent() {
        let config = GuestConfig {
            argv: vec!["/bin/sleep".into(), "30".into()],
            cwd: "/".into(),
            process: Some(pvisor_guest::ProcessConfig {
                new_session: true,
                stdin: pvisor_guest::StdinMode::Pipe,
                ..Default::default()
            }),
            ..Default::default()
        };
        let processes = Processes::default();
        let spawned = processes.spawn(&config, false).unwrap();
        let input = Arc::new(Mutex::new(spawned.input));
        let main = spawned.process;
        let pid = main.id();
        let reaper = {
            let processes = processes.clone();
            std::thread::spawn(move || processes.supervise(main))
        };
        struct Cleanup(
            Processes,
            u32,
            Option<std::thread::JoinHandle<io::Result<i32>>>,
        );
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.signal(self.1, libc::SIGKILL);
                if let Some(reaper) = self.2.take() {
                    let _ = reaper.join();
                }
            }
        }
        let _cleanup = Cleanup(processes.clone(), pid, Some(reaper));
        let (mut command, endpoint) = UnixStream::pair().unwrap();
        command
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let command_worker = {
            let config = config.clone();
            let processes = processes.clone();
            let input = input.clone();
            std::thread::spawn(move || {
                serial_loop(
                    endpoint.try_clone().unwrap(),
                    endpoint,
                    false,
                    config,
                    processes,
                    pid,
                    input,
                )
            })
        };
        let (mut signal, endpoint) = UnixStream::pair().unwrap();
        signal
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let signal_worker = std::thread::spawn(move || {
            serial_loop(
                endpoint.try_clone().unwrap(),
                endpoint,
                true,
                config,
                processes,
                pid,
                input,
            )
        });
        use pvisor_guest::control::{SerialReply, SerialRequest};
        write_frame(
            &mut command,
            &SerialRequest {
                id: 1,
                request: Request::Ping {},
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialReply>(&mut command).unwrap(),
            SerialReply {
                id: 1,
                reply: Reply::Ready
            }
        ));
        write_frame(
            &mut command,
            &SerialRequest {
                id: 2,
                request: Request::Exec {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf serial-output; sleep 30".into(),
                    ],
                    env: Default::default(),
                    unset_env: Vec::new(),
                    cwd: None,
                    user: None,
                    tty: None,
                },
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialReply>(&mut command).unwrap(),
            SerialReply {
                id: 2,
                reply: Reply::Ready
            }
        ));
        let output: SerialReply = read_frame(&mut command).unwrap();
        assert!(
            matches!(output, SerialReply { id: 2, reply: Reply::Output { stream: 1, ref bytes } } if bytes == b"serial-output")
        );
        // A second exec on the same port must finish while the first sleeps.
        write_frame(
            &mut command,
            &SerialRequest {
                id: 3,
                request: Request::Exec {
                    argv: vec!["/bin/echo".into(), "concurrent-output".into()],
                    env: Default::default(),
                    unset_env: Vec::new(),
                    cwd: None,
                    user: None,
                    tty: None,
                },
            },
        )
        .unwrap();
        let mut second = Vec::new();
        loop {
            match read_frame::<SerialReply>(&mut command).unwrap() {
                SerialReply {
                    id: 3,
                    reply: Reply::Ready,
                } => (),
                SerialReply {
                    id: 3,
                    reply: Reply::Output { bytes, .. },
                } => second.extend(bytes),
                SerialReply {
                    id: 3,
                    reply: Reply::Exit { code: 0 },
                } => break,
                reply => panic!("unexpected concurrent reply: {reply:?}"),
            }
        }
        assert_eq!(second, b"concurrent-output\n");
        use pvisor_guest::control::{ExecLaunch, SerialGuestFrame, SerialInput};
        write_frame(&mut command, &SerialRequest { id: 4, request: Request::Start {
            launch: ExecLaunch {
                argv: vec!["/bin/sh".into(), "-c".into(),
                    "test -t 0 && test -t 1 && stty size; read line; printf 'terminal:%s' \"$line\"".into()],
                env: Default::default(), replace_env: false, limits: Default::default(),
                cwd: None, user: None, tty: Some([37, 91]),
            },
        }}).unwrap();
        assert!(matches!(
            read_frame::<SerialGuestFrame>(&mut command).unwrap(),
            SerialGuestFrame::Reply(SerialReply {
                id: 4,
                reply: Reply::Started { .. }
            })
        ));
        write_frame(
            &mut command,
            &SerialInput {
                input_id: 4,
                input: StdinFrame::Data {
                    bytes: b"hello\n".to_vec(),
                },
            },
        )
        .unwrap();
        let mut terminal = Vec::new();
        let (mut ack, mut exited) = (false, false);
        while !ack || !exited {
            match read_frame::<SerialGuestFrame>(&mut command).unwrap() {
                SerialGuestFrame::InputAck(a) if a.input_ack == 4 => ack = true,
                SerialGuestFrame::Reply(SerialReply {
                    id: 4,
                    reply: Reply::Output { bytes, .. },
                }) => terminal.extend(bytes),
                SerialGuestFrame::Reply(SerialReply {
                    id: 4,
                    reply: Reply::Exit { code: 0 },
                }) => exited = true,
                frame => panic!("unexpected PTY frame: {frame:?}"),
            }
        }
        let terminal = String::from_utf8(terminal).unwrap();
        assert!(
            terminal.contains("37 91") && terminal.contains("terminal:hello"),
            "{terminal:?}"
        );
        // Main input uses the very same adapter, including explicit EOF.
        write_frame(
            &mut command,
            &SerialRequest {
                id: 5,
                request: Request::Input {},
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialGuestFrame>(&mut command).unwrap(),
            SerialGuestFrame::Reply(SerialReply {
                id: 5,
                reply: Reply::Ready
            })
        ));
        write_frame(
            &mut command,
            &SerialInput {
                input_id: 5,
                input: StdinFrame::Eof {},
            },
        )
        .unwrap();
        let (mut ack, mut closed) = (false, false);
        while !ack || !closed {
            match read_frame::<SerialGuestFrame>(&mut command).unwrap() {
                SerialGuestFrame::InputAck(a) if a.input_ack == 5 => ack = true,
                SerialGuestFrame::Reply(SerialReply {
                    id: 5,
                    reply: Reply::Ready,
                }) => closed = true,
                frame => panic!("unexpected main-input frame: {frame:?}"),
            }
        }
        write_frame(
            &mut command,
            &SerialRequest {
                id: 6,
                request: Request::Start {
                    launch: ExecLaunch {
                        argv: vec!["/bin/sleep".into(), "30".into()],
                        env: Default::default(),
                        replace_env: false,
                        limits: Default::default(),
                        cwd: None,
                        user: None,
                        tty: Some([24, 80]),
                    },
                },
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialGuestFrame>(&mut command).unwrap(),
            SerialGuestFrame::Reply(SerialReply {
                id: 6,
                reply: Reply::Started { .. }
            })
        ));
        write_frame(
            &mut command,
            &SerialInput {
                input_id: 6,
                input: StdinFrame::Data { bytes: vec![3] },
            },
        )
        .unwrap();
        let (mut ack, mut interrupted) = (false, false);
        while !ack || !interrupted {
            match read_frame::<SerialGuestFrame>(&mut command).unwrap() {
                SerialGuestFrame::InputAck(a) if a.input_ack == 6 => ack = true,
                SerialGuestFrame::Reply(SerialReply {
                    id: 6,
                    reply: Reply::Output { .. },
                }) => (),
                SerialGuestFrame::Reply(SerialReply {
                    id: 6,
                    reply: Reply::Exit { code: 130 },
                }) => interrupted = true,
                frame => panic!("unexpected interrupt frame: {frame:?}"),
            }
        }
        write_frame(
            &mut signal,
            &SerialRequest {
                id: 1,
                request: Request::Stop {
                    signal: libc::SIGTERM,
                },
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<SerialReply>(&mut signal).unwrap(),
            SerialReply {
                id: 1,
                reply: Reply::Ready
            }
        ));
        loop {
            match read_frame::<SerialReply>(&mut command).unwrap() {
                SerialReply {
                    id: 2,
                    reply: Reply::Exit { code },
                } => {
                    assert_ne!(code, 0);
                    break;
                }
                SerialReply {
                    id: 2,
                    reply: Reply::Output { .. },
                } => (),
                reply => panic!("unexpected reply: {reply:?}"),
            }
        }
        command.shutdown(std::net::Shutdown::Both).unwrap();
        signal.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(command_worker.join().unwrap().is_err());
        assert!(signal_worker.join().unwrap().is_err());
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_native_control_roundtrip() {
        reclaim_subject(None);
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_multithread_control_roundtrip() {
        for mode in ["--threads", "--thread-churn"] {
            reclaim_subject(Some(mode));
        }
    }

    fn reclaim_config(mode: Option<&str>) -> GuestConfig {
        GuestConfig {
            argv: std::iter::once(std::env::var("PVISOR_RECLAIM_SUBJECT").expect("glibc fixture"))
                .chain(mode.map(str::to_owned))
                .collect(),
            cwd: "/".into(),
            process: Some(pvisor_guest::ProcessConfig {
                allocator_reclaim: Some(
                    std::env::var("PVISOR_RECLAIM_SO")
                        .expect("preload SO")
                        .into(),
                ),
                stdin: pvisor_guest::StdinMode::Pipe,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn reclaim_subject(mode: Option<&str>) {
        let processes = Processes::default();
        let config = reclaim_config(mode);
        let mut spawned = processes.spawn(&config, true).unwrap();
        let stdout = spawned.outputs.remove(0).0;
        let main = spawned.process;
        let pid = main.id();
        let generation = main.generation();
        let registry = processes.clone();
        let reaper = std::thread::spawn(move || registry.supervise(main).unwrap());
        let result = (|| -> io::Result<serde_json::Value> {
            use std::io::BufRead;
            let mut ready = String::new();
            std::io::BufReader::new(stdout).read_line(&mut ready)?;
            if !ready.starts_with("ready pid=") {
                return Err(io::Error::other("subject did not become ready"));
            }
            assert!(
                processes
                    .reclaim_allocator(pid, generation + 1, true)
                    .is_err()
            );
            let mut report = serde_json::Value::Null;
            for trim in [false, true] {
                let (mut client, server) = UnixStream::pair()?;
                write_frame(
                    &mut client,
                    &Request::ReclaimAllocator {
                        pid,
                        generation,
                        trim,
                    },
                )?;
                serve(
                    server,
                    &config,
                    &processes,
                    pid,
                    &Arc::new(Mutex::new(None)),
                )?;
                let Reply::Value { value } = read_frame(&mut client)? else {
                    return Err(io::Error::other("missing report"));
                };
                eprintln!("allocator-reclaim {mode:?}: {value}");
                report = value;
            }
            let repeated = processes.reclaim_allocator(pid, generation, true)?;
            assert_eq!(repeated["worker_tid"], report["worker_tid"]);
            if mode.is_some() {
                let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
                let vm_before = status
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("VmSize:")?
                            .split_whitespace()
                            .next()?
                            .parse::<u64>()
                            .ok()
                    })
                    .unwrap();
                let fd_before = std::fs::read_dir(format!("/proc/{pid}/fd"))?.count();
                for _ in 0..5 {
                    processes.reclaim_allocator(pid, generation, true)?;
                }
                if mode == Some("--threads") {
                    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
                    let vm_after = status
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("VmSize:")?
                                .split_whitespace()
                                .next()?
                                .parse::<u64>()
                                .ok()
                        })
                        .unwrap();
                    if vm_after > vm_before + 1024
                        || std::fs::read_dir(format!("/proc/{pid}/fd"))?.count() != fd_before
                    {
                        return Err(io::Error::other(
                            "repeated reclaim retained mappings or file descriptors",
                        ));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            Ok(report)
        })();
        let _ = processes.signal(pid, libc::SIGTERM);
        let code = reaper.join().unwrap();
        let report = result.unwrap();
        assert_eq!(
            code, 143,
            "subject should remain alive with its live allocation intact"
        );
        assert_eq!(report["status"], "trimmed");
        assert_ne!(
            report["worker_tid"], report["pid"],
            "trim must run in the worker"
        );
        assert_eq!(report["malloc_trim_result"], 1);
        assert!(
            report["rss_anon_before_bytes"].as_u64().unwrap()
                > report["rss_anon_after_bytes"].as_u64().unwrap() + (32 << 20)
        );
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_cpu_bound_process() {
        reclaim_subject(Some("--busy"));
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_preserves_a_blocked_read() {
        use std::io::BufRead;
        let processes = Processes::default();
        let config = reclaim_config(Some("--blocked-read"));
        let mut spawned = processes.spawn(&config, true).unwrap();
        let mut output = std::io::BufReader::new(spawned.outputs.remove(0).0);
        let pid = spawned.process.id();
        let generation = spawned.process.generation();
        let registry = processes.clone();
        let reaper = std::thread::spawn(move || registry.supervise(spawned.process).unwrap());
        let result = (|| -> io::Result<()> {
            let mut line = String::new();
            output.read_line(&mut line)?;
            processes.reclaim_allocator(pid, generation, true)?;
            let mut input = spawned.input.take().unwrap();
            input.write_all(b"X")?;
            drop(input);
            line.clear();
            output.read_line(&mut line)?;
            if line != "read-ok\n" {
                return Err(io::Error::other("read was skipped or replayed incorrectly"));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = processes.signal(pid, libc::SIGTERM);
        }
        assert_eq!(reaper.join().unwrap(), 0);
        result.unwrap();
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_fork_exec_preserves_parent_control() {
        reclaim_lifecycle(false);
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_closed_channel_retires_without_stopping_target() {
        reclaim_lifecycle(true);
    }

    fn reclaim_lifecycle(close: bool) {
        use std::io::BufRead;
        let processes = Processes::default();
        let mut spawned = processes
            .spawn(&reclaim_config(Some("--lifecycle")), true)
            .unwrap();
        let mut output = std::io::BufReader::new(spawned.outputs.remove(0).0);
        let mut input = spawned.input.take().unwrap();
        let pid = spawned.process.id();
        let generation = spawned.process.generation();
        let registry = processes.clone();
        let reaper = std::thread::spawn(move || registry.supervise(spawned.process).unwrap());
        let mut line = String::new();
        let result = (|| -> io::Result<()> {
            output.read_line(&mut line)?;
            assert!(line.starts_with("ready pid="));
            let first = processes.reclaim_allocator(pid, generation, true)?;
            if close {
                input.write_all(b"C")?;
                line.clear();
                output.read_line(&mut line)?;
                assert_eq!(line, "channel-closed\n");
                assert!(processes.reclaim_allocator(pid, generation, true).is_err());
                assert!(processes.reclaim_allocator(pid, generation, true).is_err());
                input.write_all(b"R")?;
                line.clear();
                output.read_line(&mut line)?;
                assert_eq!(line, "fork-parent-ok\n");
            } else {
                for _ in 0..5 {
                    for command in *b"FXV" {
                        input.write_all(&[command])?;
                        let report = processes.reclaim_allocator(pid, generation, true)?;
                        assert_eq!(report["worker_tid"], first["worker_tid"]);
                        if command == b'X' || command == b'V' {
                            line.clear();
                            output.read_line(&mut line)?;
                            assert_eq!(
                                line,
                                if command == b'X' {
                                    "fork-exec-child-ok\n"
                                } else {
                                    "vfork-exec-child-ok\n"
                                }
                            );
                        }
                        line.clear();
                        output.read_line(&mut line)?;
                        assert_eq!(line, "fork-parent-ok\n");
                    }
                }
                input.write_all(b"E")?;
                line.clear();
                output.read_line(&mut line)?;
                assert!(line.starts_with("ready pid="));
                let report = processes.reclaim_allocator(pid, generation, true)?;
                assert_eq!(report["pid"], pid);
                assert_ne!(report["worker_tid"], first["worker_tid"]);
                assert_eq!(report["malloc_trim_result"], 1);
            }
            Ok(())
        })();
        drop(input);
        if result.is_err() {
            let _ = processes.signal(pid, libc::SIGKILL);
        }
        assert_eq!(reaper.join().unwrap(), 0);
        result.unwrap();
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_rejects_interposed_malloc() {
        use std::io::BufRead;
        let processes = Processes::default();
        let mut config = reclaim_config(None);
        config.env.insert(
            "LD_PRELOAD".into(),
            std::env::var("PVISOR_RECLAIM_INTERPOSER").unwrap(),
        );
        let mut spawned = processes.spawn(&config, true).unwrap();
        let mut line = String::new();
        std::io::BufReader::new(spawned.outputs.remove(0).0)
            .read_line(&mut line)
            .unwrap();
        let result =
            processes.reclaim_allocator(spawned.process.id(), spawned.process.generation(), true);
        let _ = processes.signal(spawned.process.id(), libc::SIGTERM);
        assert_eq!(processes.supervise(spawned.process).unwrap(), 143);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    #[ignore = "requires Linux and preload/glibc fixtures; see guest README"]
    fn allocator_reclaim_static_target_times_out_without_stopping() {
        use std::io::BufRead;
        let processes = Processes::default();
        let mut config = reclaim_config(None);
        config.argv = vec![std::env::var("PVISOR_RECLAIM_STATIC").unwrap()];
        let mut spawned = processes.spawn(&config, true).unwrap();
        let mut line = String::new();
        std::io::BufReader::new(spawned.outputs.remove(0).0)
            .read_line(&mut line)
            .unwrap();
        let result =
            processes.reclaim_allocator(spawned.process.id(), spawned.process.generation(), true);
        let _ = processes.signal(spawned.process.id(), libc::SIGTERM);
        assert_eq!(processes.supervise(spawned.process).unwrap(), 143);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn tcp_tunnel_guest_eof_releases_the_input_reader() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let application = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"response-without-client-half-close")
                .unwrap();
        });
        let (mut client, server) = UnixStream::pair().unwrap();
        let handler = std::thread::spawn(move || {
            serve(
                server,
                &GuestConfig::default(),
                &Processes::default(),
                0,
                &Arc::new(Mutex::new(None)),
            )
        });
        write_frame(&mut client, &Request::Connect { port }).unwrap();
        assert!(matches!(
            read_frame::<Reply>(&mut client).unwrap(),
            Reply::Ready
        ));
        let mut body = Vec::new();
        loop {
            match read_frame::<Reply>(&mut client).unwrap() {
                Reply::Output { bytes, .. } => body.extend(bytes),
                Reply::Exit { code: 0 } => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(body, b"response-without-client-half-close");
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        let eof = client.read(&mut [0; 1]);
        let _ = client.shutdown(std::net::Shutdown::Both);
        application.join().unwrap();
        handler.join().unwrap().unwrap();
        assert!(
            matches!(eof, Ok(0)),
            "guest tunnel input reader retained the connection: {eof:?}"
        );
    }

    #[test]
    fn tcp_tunnel_input_half_close_preserves_the_complete_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected: Vec<u8> = (0..131077).map(|i| (i * 37) as u8).collect();
        let response = expected.clone();
        let application = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut input = Vec::new();
            stream.read_to_end(&mut input).unwrap();
            assert_eq!(input, b"request");
            stream.write_all(&response).unwrap();
        });
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let handler = std::thread::spawn(move || {
            serve(
                server,
                &GuestConfig::default(),
                &Processes::default(),
                0,
                &Arc::new(Mutex::new(None)),
            )
        });
        write_frame(&mut client, &Request::Connect { port }).unwrap();
        assert!(matches!(
            read_frame::<Reply>(&mut client).unwrap(),
            Reply::Ready
        ));
        write_frame(
            &mut client,
            &StdinFrame::Data {
                bytes: b"request".to_vec(),
            },
        )
        .unwrap();
        write_frame(&mut client, &StdinFrame::Eof {}).unwrap();
        let mut actual = Vec::new();
        loop {
            match read_frame::<Reply>(&mut client).unwrap() {
                Reply::Output { bytes, .. } => actual.extend(bytes),
                Reply::Exit { code: 0 } => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(actual, expected);
        application.join().unwrap();
        handler.join().unwrap().unwrap();
    }

    #[test]
    fn tracked_exec_streams_stdin_and_signal_uses_shared_reaper_registry() {
        let processes = Processes::default();
        let config = GuestConfig {
            argv: vec!["/bin/sleep".into(), "60".into()],
            cwd: "/".into(),
            env: BTreeMap::from([("PVISOR_NATIVE_GENERATION".into(), "private".into())]),
            ..Default::default()
        };
        let main = processes.spawn(&config, false).unwrap().process;
        let main_pid = main.id();
        let reaper = processes.clone();
        let reaper = std::thread::spawn(move || reaper.supervise(main).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let guest = processes.clone();
        let launch = config.clone();
        let worker = std::thread::spawn(move || {
            serve(
                server,
                &launch,
                &guest,
                main_pid,
                &Arc::new(Mutex::new(None)),
            )
            .unwrap()
        });
        write_frame(
            &mut client,
            &Request::Start {
                launch: pvisor_guest::control::ExecLaunch {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "read line; printf '%s:%s' \"$line\" \"${PVISOR_NATIVE_GENERATION-unset}\""
                            .into(),
                    ],
                    env: BTreeMap::new(),
                    replace_env: false,
                    limits: BTreeMap::new(),
                    cwd: None,
                    user: None,
                    tty: None,
                },
            },
        )
        .unwrap();
        let Reply::Started { pid, generation } = read_frame(&mut client).unwrap() else {
            panic!("missing PID");
        };
        assert_eq!(
            processes
                .signal_tracked(pid, generation + 1, libc::SIGKILL)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        write_frame(
            &mut client,
            &StdinFrame::Data {
                bytes: b"hello\n".to_vec(),
            },
        )
        .unwrap();
        write_frame(&mut client, &StdinFrame::Eof {}).unwrap();
        let mut output = Vec::new();
        loop {
            match read_frame::<Reply>(&mut client).unwrap() {
                Reply::Output { bytes, .. } => output.extend(bytes),
                Reply::Exit { code } => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(output, b"hello:unset");
        worker.join().unwrap();
        assert_eq!(
            processes.signal(pid, 9).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        processes.signal(main_pid, libc::SIGTERM).unwrap();
        assert_eq!(reaper.join().unwrap(), 143);
    }

    #[test]
    fn tracked_exec_replaces_environment_and_limits() {
        let processes = Processes::default();
        let config = GuestConfig {
            argv: vec!["/bin/sleep".into(), "60".into()],
            cwd: "/".into(),
            env: BTreeMap::from([
                ("LAUNCH_ONLY".into(), "1".into()),
                ("SHARED".into(), "launch".into()),
                ("PVISOR_NATIVE_GENERATION".into(), "private".into()),
            ]),
            limits: BTreeMap::from([("RLIMIT_NOFILE".into(), (4096, 4096))]),
            ..Default::default()
        };
        let main = processes.spawn(&config, false).unwrap().process;
        let main_pid = main.id();
        let reaper = processes.clone();
        let reaper = std::thread::spawn(move || reaper.supervise(main).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let guest = processes.clone();
        let launch_config = config.clone();
        let worker = std::thread::spawn(move || {
            serve(
                server,
                &launch_config,
                &guest,
                main_pid,
                &Arc::new(Mutex::new(None)),
            )
            .unwrap()
        });
        write_frame(
            &mut client,
            &Request::Start {
                launch: pvisor_guest::control::ExecLaunch {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf '%s:%s:%s:%s:%s' \"${LAUNCH_ONLY-unset}\" \"${SHARED-unset}\" \"${EXTRA-unset}\" \"${PVISOR_NATIVE_GENERATION-unset}\" \"$(ulimit -Sn)\""
                            .into(),
                    ],
                    env: BTreeMap::from([
                        ("SHARED".into(), "exec".into()),
                        ("EXTRA".into(), "1".into()),
                    ]),
                    replace_env: true,
                    limits: BTreeMap::from([("RLIMIT_NOFILE".into(), (64, 128))]),
                    cwd: None,
                    user: None,
                    tty: None,
                },
            },
        )
        .unwrap();
        let Reply::Started {
            pid: _,
            generation: _,
        } = read_frame(&mut client).unwrap()
        else {
            panic!("missing PID");
        };
        write_frame(&mut client, &StdinFrame::Eof {}).unwrap();
        let mut output = Vec::new();
        loop {
            match read_frame::<Reply>(&mut client).unwrap() {
                Reply::Output { bytes, .. } => output.extend(bytes),
                Reply::Exit { code } => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        // replace_env drops the launch environment (including transport
        // keys), keeps only the exec-supplied variables, and applies the
        // per-exec rlimits instead of the launch limits.
        assert_eq!(output, b"unset:exec:1:unset:64");
        worker.join().unwrap();
        processes.signal(main_pid, libc::SIGTERM).unwrap();
        assert_eq!(reaper.join().unwrap(), 143);
    }
}

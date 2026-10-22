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
            processes.signal(pid, signal)?;
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
        Request::Exec {
            argv,
            env,
            unset_env,
            cwd,
            user,
            tty,
        } => {
            let mut config = config.clone();
            config.argv = argv;
            for key in unset_env {
                config.env.remove(&key);
            }
            config.env.extend(env);
            if let Some(cwd) = cwd {
                config.cwd = cwd;
            }
            let options = config.process.get_or_insert_default();
            if let Some(user) = user {
                options.user = Some(user);
            }
            options.tty = tty;
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
            write_frame(&mut stream, &Reply::Ready)?;
            let mut stdin = input.unwrap();
            let mut input = stream.try_clone()?;
            let exec_pid = process.id();
            let processes = processes.clone();
            std::thread::spawn(move || {
                if consume_input(&mut input, &mut stdin).is_err() {
                    let _ = processes.signal(exec_pid, libc::SIGKILL);
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
            for pump in pumps {
                let _ = pump.join();
            }
            write_frame(&mut *output.lock().unwrap(), &Reply::Exit { code })?;
            Ok(())
        }
    }
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

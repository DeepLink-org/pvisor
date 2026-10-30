//! Minimal PID 1: initialize the guest, supervise its workload, and report exit.
use pvisor_guest::{CONFIG_PATH, GuestConfig, NetworkConfig};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod control;
mod files;
mod process;
mod shares;
mod startup_profile;

// No Rust std I/O, allocation, panicking or threads are allowed here: this is
// called from a libc constructor before std::rt::init. Only PID 1 owns guest
// mounts; executing this binary on the host must have no bootstrap side effects.
#[cfg(not(test))]
pub(super) extern "C" fn bootstrap_stdio() {
    if unsafe { libc::getpid() } != 1 {
        return;
    }
    unsafe { repair_standard_fds() };
}

/// Supply only missing standard descriptors. Existing console descriptors keep
/// their identity and flags. Called before threads exist; failure is left to
/// the Rust runtime's fail-closed standard-descriptor check.
unsafe fn repair_standard_fds() {
    let missing = std::array::from_fn::<_, 3, _>(|fd| {
        (unsafe { libc::fcntl(fd as i32, libc::F_GETFD) }) < 0
            && unsafe { *libc::__errno_location() } == libc::EBADF
    });
    if !missing.iter().any(|missing| *missing) {
        return;
    }
    let mut null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if null < 0 {
        // Minimal images need not contain /dev. The kernel's automatic
        // devtmpfs mount then fails before it reaches PID 1. Mount it here;
        // initialize() also accepts EBUSY when it gets to the same mount.
        unsafe {
            libc::mkdir(c"/dev".as_ptr(), 0o755);
            libc::mount(
                c"devtmpfs".as_ptr(),
                c"/dev".as_ptr(),
                c"devtmpfs".as_ptr(),
                libc::MS_RELATIME,
                std::ptr::null(),
            );
            null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
        }
    }
    if null < 0 {
        return;
    }
    for (fd, missing) in missing.into_iter().enumerate() {
        if missing {
            // open() may itself have filled this slot. Clear CLOEXEC in that
            // case too: it must remain a standard stream across exec.
            if null == fd as i32 {
                unsafe { libc::fcntl(null, libc::F_SETFD, 0) };
            } else {
                unsafe { libc::dup2(null, fd as i32) };
            }
        }
    }
    if null > 2 {
        unsafe { libc::close(null) };
    }
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn mount(source: &str, target: &Path, kind: &str, flags: libc::c_ulong) -> io::Result<()> {
    match mount_with_options(source, target, kind, flags, None) {
        Err(error) if error.raw_os_error() == Some(libc::EBUSY) => Ok(()),
        result => result,
    }
}

fn mount_with_options(
    source: &str,
    target: &Path,
    kind: &str,
    flags: libc::c_ulong,
    options: Option<&str>,
) -> io::Result<()> {
    let source = CString::new(source)?;
    let target = CString::new(target.as_os_str().as_encoded_bytes())?;
    let kind = CString::new(kind)?;
    let options = options.map(CString::new).transpose()?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            kind.as_ptr(),
            flags,
            options
                .as_ref()
                .map_or(std::ptr::null(), |options| options.as_ptr().cast()),
        )
    };
    check(result)
}

fn named_stdio_ports(directory: &Path) -> io::Result<[Option<PathBuf>; 3]> {
    let mut ports = [None, None, None];
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ports),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = match fs::read_to_string(entry.path().join("name")) {
            Ok(name) => name,
            // The console's unnamed port has no name attribute. Named ports
            // also lack it until the host's PORT_NAME message is processed.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let fd = match name.trim() {
            "krun-stdin" => 0,
            "krun-stdout" => 1,
            "krun-stderr" => 2,
            _ => continue,
        };
        ports[fd] = Some(Path::new("/dev").join(entry.file_name()));
    }
    Ok(ports)
}

fn wait_stdio_ports(
    directory: &Path,
    required: Option<[bool; 3]>,
    timeout: Duration,
) -> io::Result<[Option<PathBuf>; 3]> {
    let deadline = Instant::now() + timeout;
    loop {
        let ports = named_stdio_ports(directory)?;
        if required.is_none_or(|required| {
            required
                .iter()
                .zip(&ports)
                .all(|(needed, port)| !needed || port.is_some())
        }) {
            return Ok(ports);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "required virtio console ports did not become ready",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn initialize(stdio_ports: Option<[bool; 3]>) -> io::Result<()> {
    let restricted = libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_RELATIME;
    for (source, target, kind, flags) in [
        ("devtmpfs", "/dev", "devtmpfs", libc::MS_RELATIME),
        ("proc", "/proc", "proc", restricted),
        ("sysfs", "/sys", "sysfs", restricted),
        ("cgroup2", "/sys/fs/cgroup", "cgroup2", restricted),
        (
            "devpts",
            "/dev/pts",
            "devpts",
            libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_RELATIME,
        ),
        (
            "tmpfs",
            "/dev/shm",
            "tmpfs",
            libc::MS_NOEXEC | libc::MS_NOSUID | libc::MS_RELATIME,
        ),
    ] {
        fs::create_dir_all(target)?;
        mount(source, Path::new(target), kind, flags)?;
    }
    if !Path::new("/dev/fd").exists() {
        std::os::unix::fs::symlink("/proc/self/fd", "/dev/fd")?;
    }
    ensure_etc_hosts_at(Path::new("/etc/hosts"))?;
    // PID 1 can already be a session leader, in which case setsid returns EPERM.
    if unsafe { libc::setsid() } < 0
        && io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
    {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        libc::ioctl(0, libc::TIOCSCTTY, 1);
    }
    let ports = wait_stdio_ports(
        Path::new("/sys/class/virtio-ports"),
        stdio_ports,
        Duration::from_secs(5),
    )?;
    for (fd, port) in ports.iter().enumerate() {
        if let Some(port) = port {
            redirect(fd as libc::c_int, port)?;
        }
    }
    // Interactive streams use the canonical tty; captured ports remain separate.
    for fd in 0..3 {
        if unsafe { libc::isatty(fd) } == 1 {
            redirect(fd, Path::new("/dev/hvc0"))?;
        }
    }
    let socket = network_socket()?;
    interface_up(socket.as_raw_fd(), "lo")?;
    Ok(())
}

/// Container images routinely ship no `/etc/hosts`: Docker and Kubernetes
/// inject it at start, but the VM guest consumes the image rootfs directly
/// and kubelet's bind mount never crosses the VM boundary. Without the
/// file, `localhost` falls through to the synthetic DNS resolver, which
/// answers every name from its egress range and turns loopback binds into
/// `EADDRNOTAVAIL`. Write the conventional loopback entries when the image
/// does not provide the file; an image-provided file always wins.
fn ensure_etc_hosts_at(path: &Path) -> io::Result<bool> {
    const DEFAULT_HOSTS: &str = "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n";
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, DEFAULT_HOSTS)?;
    Ok(true)
}

fn redirect(fd: libc::c_int, path: &Path) -> io::Result<()> {
    let file = OpenOptions::new().read(fd == 0).write(fd != 0).open(path)?;
    check(unsafe { libc::dup2(file.as_raw_fd(), fd) })
}

fn network_socket() -> io::Result<File> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    check(fd)?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn interface(name: &str) -> libc::ifreq {
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (dest, source) in request.ifr_name.iter_mut().zip(name.bytes()) {
        *dest = source as libc::c_char;
    }
    request
}

fn interface_up(fd: libc::c_int, name: &str) -> io::Result<()> {
    let mut request = interface(name);
    check(unsafe { libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut request) })?;
    unsafe {
        request.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
    }
    check(unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &request) })
}

fn address(ip: [u8; 4]) -> libc::sockaddr {
    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(ip),
        },
        sin_zero: [0; 8],
    };
    // sockaddr and sockaddr_in have identical sizes on the supported Linux targets.
    unsafe { std::mem::transmute(address) }
}

fn configure_network(config: &NetworkConfig) -> io::Result<()> {
    let socket = network_socket()?;
    let fd = socket.as_raw_fd();
    let mut request = interface("eth0");
    request.ifr_ifru.ifru_addr = address(config.address);
    check(unsafe { libc::ioctl(fd, libc::SIOCSIFADDR as _, &request) })?;
    request.ifr_ifru.ifru_netmask = address([255, 255, 255, 0]);
    check(unsafe { libc::ioctl(fd, libc::SIOCSIFNETMASK as _, &request) })?;
    interface_up(fd, "eth0")?;
    let mut route: libc::rtentry = unsafe { std::mem::zeroed() };
    route.rt_dst = address([0; 4]);
    route.rt_genmask = address([0; 4]);
    route.rt_gateway = address(config.gateway);
    route.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;
    check(unsafe { libc::ioctl(fd, libc::SIOCADDRT as _, &route) })
}

fn resource(name: &str) -> io::Result<libc::c_int> {
    let value = match name {
        "RLIMIT_AS" => libc::RLIMIT_AS,
        "RLIMIT_CORE" => libc::RLIMIT_CORE,
        "RLIMIT_CPU" => libc::RLIMIT_CPU,
        "RLIMIT_DATA" => libc::RLIMIT_DATA,
        "RLIMIT_FSIZE" => libc::RLIMIT_FSIZE,
        "RLIMIT_MEMLOCK" => libc::RLIMIT_MEMLOCK,
        "RLIMIT_NOFILE" => libc::RLIMIT_NOFILE,
        "RLIMIT_NPROC" => libc::RLIMIT_NPROC,
        "RLIMIT_RSS" => libc::RLIMIT_RSS,
        "RLIMIT_STACK" => libc::RLIMIT_STACK,
        "RLIMIT_LOCKS" => libc::RLIMIT_LOCKS,
        "RLIMIT_SIGPENDING" => libc::RLIMIT_SIGPENDING,
        "RLIMIT_MSGQUEUE" => libc::RLIMIT_MSGQUEUE,
        "RLIMIT_NICE" => libc::RLIMIT_NICE,
        "RLIMIT_RTPRIO" => libc::RLIMIT_RTPRIO,
        "RLIMIT_RTTIME" => libc::RLIMIT_RTTIME,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported limit {name}"),
            ));
        }
    };
    Ok(value as libc::c_int)
}

struct AttachedInput {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
}

impl AttachedInput {
    fn start(fd: i32, mut input: Box<dyn Write + Send>) -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while !cancelled.load(Ordering::Acquire) {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 25) };
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
                let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
                if count == 0 {
                    return Ok(());
                }
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) {
                        continue;
                    }
                    return Err(error);
                }
                let mut pending = &buffer[..count as usize];
                while !pending.is_empty() && !cancelled.load(Ordering::Acquire) {
                    match input.write(pending) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(size) => pending = &pending[size..],
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::BrokenPipe
                                || error.raw_os_error() == Some(libc::EIO) =>
                        {
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            Ok(())
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| io::Error::other("attached input pump panicked"))?
        })
    }
}

impl Drop for AttachedInput {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn workload(config: &GuestConfig) -> io::Result<i32> {
    let mut startup = startup_profile::Span::start("guest.spawn", startup_profile_enabled(config));
    let processes = process::Processes::default();
    let listener = config.control.then(control::listener).transpose()?;
    let process::Spawned {
        process,
        input,
        outputs,
    } = match processes.spawn(config, false) {
        Ok(spawned) => spawned,
        Err(error) => return process::spawn_error(&error).ok_or(error),
    };
    startup.complete();
    drop(startup);
    let pumps: Vec<_> = outputs
        .into_iter()
        .map(|(mut reader, fd)| {
            std::thread::spawn(move || {
                let mut buffer = [0; 16384];
                loop {
                    let size = process::read_chunk(&mut reader, &mut buffer)?;
                    if size == 0 {
                        return Ok::<_, io::Error>(());
                    }
                    if fd == 2 {
                        io::stderr().write_all(&buffer[..size])?;
                        io::stderr().flush()?;
                    } else {
                        io::stdout().write_all(&buffer[..size])?;
                        io::stdout().flush()?;
                    }
                }
            })
        })
        .collect();
    let mut attached_input = None;
    if let Some(listener) = listener {
        control::start(
            listener,
            config.clone(),
            processes.clone(),
            process.id(),
            input,
        );
    } else if config
        .process
        .as_ref()
        .is_some_and(|options| options.attached)
        && let Some(input) = input
    {
        attached_input = Some(AttachedInput::start(libc::STDIN_FILENO, input));
    }
    let code = processes.supervise(process);
    let input_result = attached_input
        .as_mut()
        .map(AttachedInput::finish)
        .transpose();
    let mut output_result = Ok(());
    for pump in pumps {
        let result = pump
            .join()
            .map_err(|_| io::Error::other("output pump panicked"))
            .and_then(|result| result);
        if output_result.is_ok() {
            output_result = result;
        }
    }
    input_result?;
    output_result?;
    code
}

fn startup_profile_enabled(config: &GuestConfig) -> bool {
    config
        .env
        .get("PVISOR_STARTUP_PROFILE")
        .is_some_and(|value| value == "1")
}

fn run() -> io::Result<i32> {
    // The opt-in bit lives in the virtual config, unavailable until it is read.
    // This one entry timestamp also accounts for config I/O when profiling is on.
    let entry = Instant::now();
    let mut bytes = Vec::new();
    File::open(CONFIG_PATH)?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest config exceeds 1 MiB",
        ));
    }
    let config: GuestConfig = serde_json::from_slice(&bytes)?;
    let profiling = startup_profile_enabled(&config);
    let mut config_span = startup_profile::Span::since("guest.config", profiling.then_some(entry));
    config.command()?; // Validate before any configuration side effects.
    config_span.complete();
    let mut startup = startup_profile::Span::start("guest.initialize", profiling);
    initialize(config.stdio_ports)?;
    startup.complete();
    // Config I/O ends before initialization, but emit only after stderr is ready.
    drop(config_span);
    drop(startup);
    let mut startup = startup_profile::Span::start("guest.mounts_network", profiling);
    if let Some(scratch) = &config.temporary_filesystem {
        // Never cover image contents or an existing guest mount. Only this
        // newly created directory may become the private scratch filesystem.
        fs::create_dir(&scratch.path)?;
        mount_with_options(
            "tmpfs",
            &scratch.path,
            "tmpfs",
            libc::MS_NODEV | libc::MS_NOSUID | libc::MS_RELATIME,
            Some(&format!("size={},mode=1777", scratch.size_bytes)),
        )?;
    }
    if let Some(target) = &config.workspace {
        mount("pvisor-workspace", target, "virtiofs", 0)?;
    }
    if let Some(target) = &config.payloads_dir {
        // Read-only shared payloads: the device enforces immutability, the
        // mount adds `ro,dax=inode` so guest reads map the host copy
        // directly instead of duplicating it into guest page cache. A
        // failed mount is fatal — staged symlinks would dangle.
        fs::create_dir_all(target)?;
        mount_with_options(
            "pvisor-payloads",
            target,
            "virtiofs",
            libc::MS_RDONLY,
            Some("dax=inode"),
        )?;
    }
    for share in &config.shares {
        // Pod volume mounts the VM tier re-expresses as shares. Read-only
        // datasets deliberately use `dax=never`: ordinary reads stream
        // through the one host page-cache copy with no DAX-window coupling
        // to dataset size. A failed mount is fatal — the workload expects
        // the volume to exist.
        fs::create_dir_all(&share.path)?;
        if share.read_only {
            mount_with_options(
                &share.tag,
                &share.path,
                "virtiofs",
                libc::MS_RDONLY,
                Some("dax=never"),
            )?;
        } else {
            mount(&share.tag, &share.path, "virtiofs", 0)?;
        }
    }
    if let Some(network) = &config.network {
        configure_network(network)?;
    }
    startup.complete();
    drop(startup);
    let mut startup = startup_profile::Span::start("guest.helpers", profiling);
    if let Some(argv) = &config.agent {
        let program = argv
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty agent argv"))?;
        Command::new(program)
            .args(&argv[1..])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    // Fatal guest setup finishes before any advisory process spawns: a
    // failed mount must abort the launch without leaking sidecars.
    shares::install(&config)?;
    for sidecar in &config.sidecars {
        // Advisory daemons: a spawn or log failure is reported on the
        // console and never aborts the workload. stderr (and stdout) append
        // to the sidecar's log inside the guest root; the workload's task
        // IO is never shared.
        match config.sidecar_command(sidecar) {
            Ok(mut command) => {
                let null = || (Stdio::null(), Stdio::null());
                let stdio = match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&sidecar.log)
                {
                    Ok(log) => match log.try_clone() {
                        Ok(copy) => (Stdio::from(log), Stdio::from(copy)),
                        Err(error) => {
                            eprintln!(
                                "pvisor guest: sidecar log {}: {error}",
                                sidecar.log.display()
                            );
                            null()
                        }
                    },
                    Err(error) => {
                        eprintln!(
                            "pvisor guest: sidecar log {}: {error}",
                            sidecar.log.display()
                        );
                        null()
                    }
                };
                // Sidecar env is the workload environment overlaid by the
                // payload's own; give it the same Docker-style HOME
                // defaulting the workload gets (root's passwd entry) so
                // command-execution daemons — and the shells they spawn —
                // see a usable HOME.
                if !config.env.contains_key("HOME") && !sidecar.env.contains_key("HOME") {
                    command.env(
                        "HOME",
                        process::default_home(
                            0,
                            &fs::read_to_string("/etc/passwd").unwrap_or_default(),
                        ),
                    );
                }
                if let Err(error) = command
                    .stdin(Stdio::null())
                    .stdout(stdio.0)
                    .stderr(stdio.1)
                    .spawn()
                {
                    eprintln!(
                        "pvisor guest: sidecar {:?} failed to start: {error}",
                        sidecar.argv.first()
                    );
                }
            }
            Err(error) => eprintln!("pvisor guest: invalid sidecar: {error}"),
        }
    }
    for bridge in &config.bridges {
        // Advisory relays: failures report on the console and never affect
        // the workload (the host executor's probes surface the outage).
        match vsock_listen(bridge.vsock_port) {
            Ok(listener) => {
                let tcp_port = bridge.tcp_port;
                let vsock_port = bridge.vsock_port;
                std::thread::spawn(move || run_port_bridge(listener, tcp_port, vsock_port));
            }
            Err(error) => eprintln!(
                "pvisor guest: bridge vsock {} listener failed: {error}",
                bridge.vsock_port
            ),
        }
    }
    startup.complete();
    drop(startup);
    workload(&config)
}

/// Accept loop of one port bridge: every AF_VSOCK connection is pumped to
/// guest 127.0.0.1:tcp_port and back. One guest TCP connection per vsock
/// connection; both directions share it.
fn run_port_bridge(listener: std::os::fd::OwnedFd, tcp_port: u16, vsock_port: u32) {
    loop {
        let client = unsafe {
            libc::accept4(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if client < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("pvisor guest: bridge vsock {vsock_port} accept: {error}");
            return;
        }
        let client = unsafe { std::os::fd::OwnedFd::from_raw_fd(client) };
        let service = match std::net::TcpStream::connect(("127.0.0.1", tcp_port)) {
            Ok(service) => service,
            Err(error) => {
                eprintln!(
                    "pvisor guest: bridge vsock {vsock_port} to 127.0.0.1:{tcp_port}: {error}"
                );
                continue;
            }
        };
        std::thread::spawn(move || {
            let _ = pump_vsock_to_tcp(client, service);
        });
    }
}

/// Byte pump for one bridged connection: vsock ↔ guest TCP.
fn pump_vsock_to_tcp(vsock: std::os::fd::OwnedFd, service: std::net::TcpStream) -> io::Result<()> {
    let mut vsock_read = unsafe { File::from_raw_fd(libc::dup(vsock.as_raw_fd())) };
    let mut vsock_write = unsafe { File::from_raw_fd(libc::dup(vsock.as_raw_fd())) };
    drop(vsock);
    let mut service = service;
    let mut service_write = service.try_clone()?;
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut vsock_read, &mut service_write);
        let _ = service_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut service, &mut vsock_write);
    let _ = up.join();
    Ok(())
}

/// Listen on a guest AF_VSOCK port (host connections arrive through the
/// executor's vsock tunnel). AF_VSOCK has no std API; the raw syscall shape
/// matches AF_UNIX.
fn vsock_listen(port: u32) -> io::Result<std::os::fd::OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    check(fd)?;
    let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    address.svm_family = libc::AF_VSOCK as u16;
    // VMADDR_CID_ANY (-1): any context may connect.
    address.svm_cid = u32::MAX;
    address.svm_port = port;
    let bound = unsafe {
        libc::bind(
            fd,
            (&mut address as *mut libc::sockaddr_vm).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if bound != 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    if unsafe { libc::listen(fd, 16) } != 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

pub fn main() {
    if unsafe { libc::getpid() } != 1 {
        eprintln!("pvisor-guest must run as PID 1");
        std::process::exit(125);
    }
    let root = File::open("/").expect("open root to report guest exit");
    let token = (|| -> io::Result<_> {
        check(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) })?;
        let mut token = [0u8; pvisor_guest::exit_status::TOKEN_BYTES];
        check(unsafe {
            libc::ioctl(
                root.as_raw_fd(),
                pvisor_guest::exit_status::CLAIM_IOCTL as _,
                token.as_mut_ptr(),
            )
        })?;
        Ok(token)
    })();
    let mut code = match token
        .as_ref()
        .map_err(|error| io::Error::other(error.to_string()))
        .and_then(|_| run())
    {
        Ok(code) => code,
        Err(error) => {
            eprintln!("pvisor guest: {error}");
            // Keep an early initialization error visible in an owned rootfs
            // even when console ports have not been configured yet.
            let _ = fs::write("/.pvisor-guest-error", error.to_string());
            125
        }
    };
    if let Err(error) = sync_virtiofs_mounts() {
        eprintln!("guest writeback failed: {error}");
        code = 125;
    }
    unsafe { libc::sync() };
    if let Ok(token) = token {
        let report = pvisor_guest::exit_status::report(&token, code).and_then(|bytes| {
            check(unsafe {
                libc::ioctl(
                    root.as_raw_fd(),
                    pvisor_guest::exit_status::REPORT_IOCTL as _,
                    bytes.as_ptr(),
                )
            })
        });
        if let Err(error) = report {
            eprintln!("report guest exit: {error}");
        }
    }
    unsafe {
        libc::reboot(libc::RB_AUTOBOOT);
    }
    // A failed reboot must not let PID 1 exit and leave an unexplained kernel panic.
    eprintln!("guest reboot failed: {}", io::Error::last_os_error());
    loop {
        unsafe {
            libc::pause();
        }
    }
}

fn sync_virtiofs_mounts() -> io::Result<()> {
    let mut mounts = Vec::new();
    File::open("/proc/self/mounts")?
        .take((1 << 20) + 1)
        .read_to_end(&mut mounts)?;
    let mut failed = None;
    for path in crate::mounts::virtiofs_mounts(&mounts)? {
        let result =
            File::open(&path).and_then(|file| check(unsafe { libc::syncfs(file.as_raw_fd()) }));
        if let Err(error) = result {
            failed.get_or_insert_with(|| io::Error::other(format!("{}: {error}", path.display())));
        }
    }
    failed.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_input_cancel_joins_without_stdin_eof_or_output_capacity() {
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};
        let (mut source, reader) = UnixStream::pair().unwrap();
        struct Blocked(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Write for Blocked {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0.store(true, std::sync::atomic::Ordering::Release);
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let blocked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        source.write_all(b"pending").unwrap();
        let mut pump = AttachedInput::start(reader.as_raw_fd(), Box::new(Blocked(blocked.clone())));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !blocked.load(std::sync::atomic::Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let start = Instant::now();
        pump.finish().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        // Also cancel a pump waiting on an idle bootstrap descriptor.
        let (_source, reader) = UnixStream::pair().unwrap();
        let (writer, _sink) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let mut pump = AttachedInput::start(reader.as_raw_fd(), Box::new(writer));
        pump.finish().unwrap();
    }

    #[test]
    fn attached_input_eof_closes_child_pipe() {
        use std::os::unix::net::UnixStream;
        let (mut source, reader) = UnixStream::pair().unwrap();
        let (writer, mut sink) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        sink.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut pump = AttachedInput::start(reader.as_raw_fd(), Box::new(writer));
        source.write_all(b"hello").unwrap();
        drop(source);
        let mut bytes = Vec::new();
        sink.read_to_end(&mut bytes).unwrap();
        pump.finish().unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn etc_hosts_defaults_only_when_the_image_ships_none() {
        let temporary = tempfile::tempdir().unwrap();
        // An image-provided hosts file always wins.
        let provided = temporary.path().join("provided/hosts");
        fs::create_dir_all(provided.parent().unwrap()).unwrap();
        fs::write(&provided, "10.0.0.1 custom\n").unwrap();
        assert!(!ensure_etc_hosts_at(&provided).unwrap());
        assert_eq!(fs::read_to_string(&provided).unwrap(), "10.0.0.1 custom\n");
        // A missing file gets the conventional loopback entries for both
        // families, with the parent directory created when absent.
        let missing = temporary.path().join("nested/etc/hosts");
        assert!(ensure_etc_hosts_at(&missing).unwrap());
        let written = fs::read_to_string(&missing).unwrap();
        assert!(written.contains("127.0.0.1\tlocalhost"));
        assert!(written.contains("::1\tlocalhost ip6-localhost ip6-loopback"));
    }

    #[test]
    fn bootstrap_repairs_every_closed_stdio_combination_without_changing_open_fds() {
        const CHILD: &str = "PVISOR_TEST_BOOTSTRAP_STDIO";
        if std::env::var_os(CHILD).is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "linux::tests::bootstrap_repairs_every_closed_stdio_combination_without_changing_open_fds",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        // Run in an isolated process: neither another test nor a Rust thread
        // may open descriptors between close() and this pre-runtime repair.
        let saved = std::array::from_fn::<_, 3, _>(|fd| unsafe {
            libc::fcntl(fd as i32, libc::F_DUPFD_CLOEXEC, 3)
        });
        assert!(saved.iter().all(|fd| *fd > 2));
        for mask in 0..8 {
            for fd in 0..3 {
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                    0
                );
                if mask & (1 << fd) != 0 {
                    assert_eq!(unsafe { libc::close(fd) }, 0);
                }
            }
            unsafe { repair_standard_fds() };
            for fd in 0..3 {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                let missing = mask & (1 << fd) != 0;
                assert_eq!(flags, if missing { 0 } else { libc::FD_CLOEXEC });
                let mut actual: libc::stat = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { libc::fstat(fd, &mut actual) }, 0);
                if missing {
                    assert_eq!(actual.st_mode & libc::S_IFMT, libc::S_IFCHR);
                    assert_eq!(libc::major(actual.st_rdev), 1);
                    assert_eq!(libc::minor(actual.st_rdev), 3);
                } else {
                    let mut original: libc::stat = unsafe { std::mem::zeroed() };
                    assert_eq!(unsafe { libc::fstat(saved[fd as usize], &mut original) }, 0);
                    assert_eq!(
                        (actual.st_dev, actual.st_ino),
                        (original.st_dev, original.st_ino)
                    );
                }
                assert_eq!(unsafe { libc::dup2(saved[fd as usize], fd) }, fd);
            }
        }
        for fd in saved {
            assert_eq!(unsafe { libc::close(fd) }, 0);
        }
    }

    #[test]
    fn stdio_waits_for_late_port_names_and_new_ports() {
        let directory = tempfile::tempdir().unwrap();
        let class = directory.path().join("virtio-ports");
        fs::create_dir(&class).unwrap();
        // The unnamed console exists first. A named port can exist before
        // its name attribute, and another port can be added later still.
        fs::create_dir(class.join("vport0p0")).unwrap();
        fs::create_dir(class.join("vport0p1")).unwrap();
        let publish = class.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            fs::write(publish.join("vport0p1/name"), "krun-stdout\n").unwrap();
            std::thread::sleep(Duration::from_millis(10));
            fs::create_dir(publish.join("vport0p2")).unwrap();
            fs::write(publish.join("vport0p2/name"), "krun-stderr\n").unwrap();
        });
        let ports =
            wait_stdio_ports(&class, Some([false, true, true]), Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(
            ports,
            [
                None,
                Some("/dev/vport0p1".into()),
                Some("/dev/vport0p2".into())
            ]
        );
    }

    #[test]
    fn required_missing_port_fails_instead_of_using_the_console() {
        let directory = tempfile::tempdir().unwrap();
        let error = wait_stdio_ports(directory.path(), Some([false, true, false]), Duration::ZERO)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            wait_stdio_ports(
                &directory.path().join("not-created"),
                Some([false; 3]),
                Duration::ZERO,
            )
            .unwrap(),
            [None, None, None]
        );
    }

    #[test]
    fn workload_preserves_exit_signal_and_limits() {
        let mut config = GuestConfig {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "test \"$(ulimit -n)\" = 32 || exit 41; exit 7".into(),
            ],
            cwd: "/".into(),
            limits: std::collections::BTreeMap::from([("RLIMIT_NOFILE".into(), (32, 32))]),
            ..Default::default()
        };
        assert_eq!(workload(&config).unwrap(), 7);
        config.argv[2] = "exit 0".into();
        assert_eq!(workload(&config).unwrap(), 0);
        config.argv[2] = "kill -TERM $$".into();
        assert_eq!(workload(&config).unwrap(), 143);
        config.argv = vec!["/pvisor-no-such-program".into()];
        assert_eq!(workload(&config).unwrap(), 127);
    }
}

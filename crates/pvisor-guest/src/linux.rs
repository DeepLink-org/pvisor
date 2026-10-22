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
mod process;
mod shares;

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

fn workload(config: &GuestConfig) -> io::Result<i32> {
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
    let pumps: Vec<_> = outputs
        .into_iter()
        .map(|(mut reader, _)| {
            std::thread::spawn(move || {
                let mut buffer = [0; 16384];
                while let Ok(size) = process::read_chunk(&mut reader, &mut buffer) {
                    if size == 0 || io::stdout().write_all(&buffer[..size]).is_err() {
                        break;
                    }
                    let _ = io::stdout().flush();
                }
            })
        })
        .collect();
    if let Some(listener) = listener {
        control::start(
            listener,
            config.clone(),
            processes.clone(),
            process.id(),
            input,
        );
    }
    let code = processes.supervise(process)?;
    for pump in pumps {
        let _ = pump.join();
    }
    Ok(code)
}

fn run() -> io::Result<i32> {
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
    config.command()?; // Validate before any configuration side effects.
    initialize(config.stdio_ports)?;
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
    let mut code = match run() {
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
    if unsafe { libc::ioctl(root.as_raw_fd(), 0x7602, code) } < 0 {
        eprintln!("report guest exit: {}", io::Error::last_os_error());
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

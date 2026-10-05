//! Minimal PID 1: initialize the guest, supervise its workload, and report exit.
use pvisor_guest::{CONFIG_PATH, GuestConfig, NetworkConfig};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    let limits = config
        .limits
        .iter()
        .map(|(name, (soft, hard))| {
            Ok((
                resource(name)?,
                libc::rlimit {
                    rlim_cur: *soft,
                    rlim_max: *hard,
                },
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut command = config.command()?;
    unsafe {
        command.pre_exec(move || {
            for (resource, limit) in &limits {
                // syscall's varargs avoid glibc vs musl setrlimit argument type differences.
                check(libc::syscall(
                    libc::SYS_prlimit64,
                    0,
                    *resource,
                    limit,
                    std::ptr::null::<libc::rlimit>(),
                ) as libc::c_int)?;
            }
            Ok(())
        });
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(127),
        Err(error) if matches!(error.raw_os_error(), Some(libc::EACCES | libc::ENOEXEC)) => {
            return Ok(126);
        }
        Err(error) => return Err(error),
    };
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        if pid == child.id() as libc::pid_t {
            return Ok(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            });
        }
    }
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
    workload(&config)
}

pub fn main() {
    if unsafe { libc::getpid() } != 1 {
        eprintln!("pvisor-guest must run as PID 1");
        std::process::exit(125);
    }
    let root = File::open("/").expect("open root to report guest exit");
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("pvisor guest: {error}");
            // Keep an early initialization error visible in an owned rootfs
            // even when console ports have not been configured yet.
            let _ = fs::write("/.pvisor-guest-error", error.to_string());
            125
        }
    };
    if unsafe { libc::ioctl(root.as_raw_fd(), 0x7602, code) } < 0 {
        eprintln!("report guest exit: {}", io::Error::last_os_error());
    }
    unsafe {
        libc::sync();
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

//! Guest-only supervisor and byte-transparent AF_VSOCK adapter for real execd.
//! Built separately by build.rs; no host execution or shell evaluation.
use std::{
    fs::File,
    io,
    net::TcpStream,
    os::fd::{AsRawFd, FromRawFd},
    process::{Command, ExitCode},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const SockAddrVm, len: u32) -> i32;
    fn listen(fd: i32, backlog: i32) -> i32;
    fn accept4(fd: i32, addr: *mut u8, len: *mut u32, flags: i32) -> i32;
    fn shutdown(fd: i32, how: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
}
#[repr(C)]
struct SockAddrVm {
    family: u16,
    reserved: u16,
    port: u32,
    cid: u32,
    zero: [u8; 4],
}
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

fn listener() -> io::Result<File> {
    // Linux AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC. Guest CID is fixed by pVisor.
    let fd = unsafe { socket(40, 1 | 0x80000, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let owner = unsafe { File::from_raw_fd(fd) };
    let addr = SockAddrVm {
        family: 40,
        reserved: 0,
        port: 44772,
        cid: 3,
        zero: [0; 4],
    };
    if unsafe { bind(fd, &addr, 16) } < 0 || unsafe { listen(fd, 128) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(owner)
}
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
fn bridge(listener: File) -> io::Result<()> {
    let slots = Arc::new(AtomicUsize::new(0));
    loop {
        let fd = unsafe {
            accept4(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0x80000,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let guest = unsafe { File::from_raw_fd(fd) };
        if slots.fetch_add(1, Ordering::Relaxed) >= 128 {
            slots.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let slot = Slot(slots.clone());
        thread::spawn(move || {
            let _slot = slot;
            let Ok(mut tcp) = TcpStream::connect_timeout(
                &"127.0.0.1:44772".parse().unwrap(),
                Duration::from_secs(2),
            ) else {
                return;
            };
            let _ = tcp.set_nodelay(true);
            let Ok(mut tcp_write) = tcp.try_clone() else {
                return;
            };
            let Ok(mut guest_read) = guest.try_clone() else {
                return;
            };
            let mut guest_write = guest;
            thread::scope(|scope| {
                scope.spawn(move || {
                    let _ = io::copy(&mut guest_read, &mut tcp_write);
                    let _ = tcp_write.shutdown(std::net::Shutdown::Write);
                });
                let _ = io::copy(&mut tcp, &mut guest_write);
                unsafe {
                    shutdown(guest_write.as_raw_fd(), 1);
                }
            });
        });
    }
}
fn run() -> io::Result<u8> {
    unsafe {
        signal(15, stop);
        signal(2, stop);
    }
    let socket = listener()?;
    thread::spawn(move || {
        if bridge(socket).is_err() {
            STOP.store(true, Ordering::Relaxed);
        }
    });
    let mut child = Command::new("/opt/pvisor/execd")
        .args(["--runtime-init", "--init", "--"])
        .args(std::env::args_os().skip(1))
        .env_remove("PVISOR_EXECD_ACCESS_TOKEN")
        .env_remove("EXECD_ACCESS_TOKEN")
        .env("EXECD_RUNTIME_INIT", "true")
        .spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(1) as u8);
        }
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        kill(child.id() as i32, 15);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(0);
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill()?;
    child.wait()?;
    Ok(0)
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("sandbox adapter: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod mounts;

// libc invokes constructors before Rust's std runtime checks fd 0..2. The
// kernel's initial console can be unavailable, even though virtio ports will
// become ready later. PID 1 must supply /dev/null before that check can abort.
#[cfg(all(target_os = "linux", not(test)))]
#[used]
#[unsafe(link_section = ".init_array")]
static BOOTSTRAP_STDIO: extern "C" fn() = linux::bootstrap_stdio;

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("pvisor-guest runs inside a Linux VM");
        std::process::exit(125);
    }
}

#[cfg(target_os = "linux")]
mod linux;

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("pvisor-guest runs inside a Linux VM");
        std::process::exit(125);
    }
}

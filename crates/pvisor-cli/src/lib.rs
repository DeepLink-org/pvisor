//! Application frontends for pVisor. Embed `pvisor` for runtime and Session APIs.
#[cfg(not(windows))]
pub mod cli;
#[cfg(windows)]
#[path = "windows_cli.rs"]
pub mod cli;
#[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
pub mod companions;
mod container_args;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod vm_run;
#[cfg(any(windows, test))]
mod windows_terminal;
// Parse the actual Windows frontend on Unix test hosts as well. This does not
// run WHP or substitute a fake container store for hardware acceptance.
#[cfg(all(
    test,
    not(windows),
    not(all(target_os = "macos", target_arch = "x86_64"))
))]
#[path = "windows_cli.rs"]
#[allow(dead_code)]
mod windows_cli_tests;

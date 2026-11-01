//! Application frontends for pVisor. Embed `pvisor` for runtime and Job APIs.
#[cfg(not(windows))]
pub mod cli;
#[cfg(windows)]
#[path = "windows_cli.rs"]
pub mod cli;
#[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
pub mod companions;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod vm_run;

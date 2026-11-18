//! Application frontends for pVisor. Embed `pvisor` for runtime and Session APIs.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod api;
mod application;
#[cfg(not(windows))]
mod cli;
#[cfg(windows)]
#[path = "windows_cli.rs"]
mod cli;
#[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
mod companions;
mod container_args;
#[cfg(unix)]
mod replay;
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

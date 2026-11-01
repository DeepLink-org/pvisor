//! Ordinary terminal availability and process output initialization.

pub fn init_output() {
    pvisor::cache::progress::init_output(None);
    pvisor::diagnostics::init(None);
}

pub fn available() -> bool {
    (unsafe { libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1 })
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

//! Linux PID 1 trampoline: close stdio before exec, without Rust std startup.
#![no_std]
#![no_main]

#[link(name = "c")]
unsafe extern "C" {
    fn close(fd: i32) -> i32;
    fn execve(
        path: *const core::ffi::c_char,
        argv: *const *const core::ffi::c_char,
        envp: *const *const core::ffi::c_char,
    ) -> i32;
}

#[unsafe(no_mangle)]
extern "C" fn main() -> i32 {
    let argv = [c"/init-real".as_ptr(), core::ptr::null()];
    let env = [core::ptr::null()];
    // SAFETY: this single-threaded test-owned PID 1 deliberately relinquishes
    // fd 0..2. Both pointer arrays are live and terminated until exec returns.
    unsafe {
        for fd in 0..3 {
            close(fd);
        }
        execve(argv[0], argv.as_ptr(), env.as_ptr());
    }
    125
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

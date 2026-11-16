//! Test-only malloc interposition: forward to glibc while changing symbol identity.
#![no_std]
use core::ffi::c_void;
#[link(name = "libc.so.6", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn __libc_malloc(size: usize) -> *mut c_void;
}
#[unsafe(no_mangle)]
unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    // SAFETY: preserve glibc malloc's allocation and failure semantics exactly.
    unsafe { __libc_malloc(size) }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: no unwinding may cross the interposed C ABI.
    unsafe { libc::abort() }
}

//! Minimal glibc executable startup for test fixtures, without C startup objects.
use core::{
    ffi::{c_char, c_void},
    ptr,
};

#[link(name = "libc.so.6", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn __libc_start_main(
        main: unsafe extern "C" fn(i32, *mut *mut c_char) -> i32,
        argc: i32,
        argv: *mut *mut c_char,
        init: *const c_void,
        fini: *const c_void,
        rtld_fini: *const c_void,
        stack_end: *const c_void,
    ) -> !;
    fn fixture_main(argc: i32, argv: *mut *mut c_char) -> i32;
}
#[link(name = "libpthread.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: fixture failures terminate instead of unwinding across glibc.
    unsafe { libc::abort() }
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".global _start",
    ".type _start,%function",
    "_start:",
    "mov x0, sp",
    "mov x1, x0",
    "b fixture_start"
);
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".global _start",
    ".type _start,@function",
    "_start:",
    "mov rdi, rsp",
    "and rsp, -16",
    "call fixture_start",
    "ud2"
);

#[unsafe(no_mangle)]
unsafe extern "C" fn fixture_start(stack: *const usize) -> ! {
    // SAFETY: the ELF loader supplies argc/argv on the initial stack. glibc initializes
    // its runtime and calls the fixture's C-ABI main; no C startup objects are linked.
    unsafe {
        __libc_start_main(
            fixture_main,
            *stack as i32,
            stack.add(1).cast_mut().cast(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            stack.cast(),
        )
    }
}

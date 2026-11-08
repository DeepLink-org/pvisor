//! Glibc allocator/lifecycle fixture. Allocations deliberately bypass Rust's allocator.
#![no_std]
#![no_main]
mod fixture_runtime;
use core::{
    ffi::{CStr, c_char, c_void},
    mem::MaybeUninit,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};
unsafe extern "C" {
    static mut environ: *mut *mut c_char;
}
static PROGRESS: AtomicU64 = AtomicU64::new(0);

fn channel_descriptor() -> i32 {
    // SAFETY: the environment string is read-only and atoi accepts its fd prefix.
    unsafe {
        let value = libc::getenv(c"PVISOR_RECLAIM_CHANNEL".as_ptr());
        if value.is_null() {
            -1
        } else {
            libc::atoi(value)
        }
    }
}
fn thread_count() -> i32 {
    // SAFETY: the directory owns each dirent until the next readdir; it is closed once.
    unsafe {
        let dir = libc::opendir(c"/proc/self/task".as_ptr());
        if dir.is_null() {
            return -1;
        }
        let mut count = 0;
        loop {
            let entry = libc::readdir(dir);
            if entry.is_null() {
                break;
            }
            if (*entry).d_name[0] != b'.' as c_char {
                count += 1;
            }
        }
        libc::closedir(dir);
        count
    }
}
extern "C" fn short_worker(_: *mut c_void) -> *mut c_void {
    // SAFETY: volatile access stays within a live glibc allocation, freed once.
    unsafe {
        let memory = libc::malloc(4096).cast::<u8>();
        if memory.is_null() {
            libc::abort();
        }
        memory.write_volatile(0x5a);
        libc::free(memory.cast());
    }
    ptr::null_mut()
}
extern "C" fn worker(_: *mut c_void) -> *mut c_void {
    loop {
        // SAFETY: all accesses are within the live allocation; volatile preserves the memory oracle.
        unsafe {
            let memory = libc::malloc(65536).cast::<u8>();
            if memory.is_null() {
                libc::abort();
            }
            for i in (0..65536).step_by(4096) {
                memory.add(i).write_volatile(0xa5);
            }
            for i in (0..65536).step_by(4096) {
                if memory.add(i).read_volatile() != 0xa5 {
                    libc::abort();
                }
            }
            libc::free(memory.cast());
            PROGRESS.fetch_add(1, Ordering::Relaxed);
            libc::usleep(1000);
        }
    }
}
extern "C" fn churn(_: *mut c_void) -> *mut c_void {
    loop {
        // SAFETY: join consumes only a successfully created thread; no Rust state crosses it.
        unsafe {
            let mut thread = MaybeUninit::uninit();
            if libc::pthread_create(
                thread.as_mut_ptr(),
                ptr::null(),
                short_worker,
                ptr::null_mut(),
            ) != 0
                || libc::pthread_join(thread.assume_init(), ptr::null_mut()) != 0
            {
                libc::abort();
            }
            libc::usleep(10000);
        }
    }
}

// vfork's child shares the parent's stack. Keep its entire exec/_exit path in
// assembly so it never enters a Rust frame or mutates parent stack locals.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn vfork_exec(
    _: *const c_char,
    _: *const *const c_char,
    _: *const *const c_char,
) -> i32 {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-48]!",
        "stp x19, x20, [sp, #16]",
        "str x21, [sp, #32]",
        "mov x19, x0",
        "mov x20, x1",
        "mov x21, x2",
        "bl vfork",
        "cbnz x0, 2f",
        "mov x0, x19",
        "mov x1, x20",
        "mov x2, x21",
        "bl execve",
        "mov x0, #16",
        "bl _exit",
        "2:",
        "ldr x21, [sp, #32]",
        "ldp x19, x20, [sp, #16]",
        "ldp x29, x30, [sp], #48",
        "ret"
    );
}
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn vfork_exec(
    _: *const c_char,
    _: *const *const c_char,
    _: *const *const c_char,
) -> i32 {
    core::arch::naked_asm!(
        "push r12",
        "push r13",
        "push r14",
        "mov r12, rdi",
        "mov r13, rsi",
        "mov r14, rdx",
        "call vfork@PLT",
        "test eax, eax",
        "jnz 2f",
        "mov rdi, r12",
        "mov rsi, r13",
        "mov rdx, r14",
        "call execve@PLT",
        "mov edi, 16",
        "call _exit@PLT",
        "2:",
        "pop r14",
        "pop r13",
        "pop r12",
        "ret"
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn fixture_main(argc: i32, argv: *mut *mut c_char) -> i32 {
    // SAFETY: glibc supplies argc NUL-terminated argv strings. All allocation accesses
    // are bounded and volatile; fork children use only async-signal-safe operations.
    unsafe {
        let mode = if argc == 2 {
            CStr::from_ptr(*argv.add(1)).to_bytes()
        } else {
            b""
        };
        if mode == b"--child-exec-check" {
            if thread_count() != 1 || libc::fcntl(channel_descriptor(), libc::F_GETFD) < 0 {
                return 5;
            }
            libc::puts(c"fork-exec-child-ok".as_ptr());
            return 0;
        }
        if mode == b"--vfork-exec-check" {
            if thread_count() != 1
                || libc::fcntl(channel_descriptor(), libc::F_GETFD) >= 0
                || *libc::__errno_location() != libc::EBADF
            {
                return 15;
            }
            libc::puts(c"vfork-exec-child-ok".as_ptr());
            return 0;
        }
        let busy = mode == b"--busy";
        let multithread = mode == b"--threads";
        let churn_threads = mode == b"--thread-churn";
        let lifecycle = mode == b"--lifecycle" || mode == b"--after-exec";
        let blocked_read = mode == b"--blocked-read";
        if argc > 1 && !busy && !multithread && !churn_threads && !lifecycle && !blocked_read {
            return 2;
        }
        if libc::mallopt(libc::M_TRIM_THRESHOLD, -1) == 0 || libc::mallopt(libc::M_MMAP_MAX, 0) == 0
        {
            return 1;
        }
        let size = 64 * 1024 * 1024;
        let memory = libc::malloc(size).cast::<u8>();
        let live = libc::malloc(4096).cast::<u8>();
        if memory.is_null() || live.is_null() {
            libc::free(memory.cast());
            libc::free(live.cast());
            return 1;
        }
        for i in 0..size {
            memory.add(i).write_volatile(0x5a);
        }
        for i in 0..4096 {
            live.add(i).write_volatile(0xa5);
        }
        libc::free(memory.cast());
        if multithread || churn_threads || lifecycle {
            for _ in 0..4 {
                let mut thread = MaybeUninit::uninit();
                if libc::pthread_create(thread.as_mut_ptr(), ptr::null(), worker, ptr::null_mut())
                    != 0
                {
                    return 1;
                }
            }
            if churn_threads {
                let mut thread = MaybeUninit::uninit();
                if libc::pthread_create(thread.as_mut_ptr(), ptr::null(), churn, ptr::null_mut())
                    != 0
                {
                    return 1;
                }
            }
        }
        libc::printf(
            c"ready pid=%ld progress=%p\n".as_ptr(),
            i64::from(libc::getpid()),
            &PROGRESS,
        );
        libc::fflush(ptr::null_mut());
        let mut command = 0u8;
        if blocked_read {
            let count = loop {
                let n = libc::read(0, (&mut command as *mut u8).cast(), 1);
                if n >= 0 || *libc::__errno_location() != libc::EINTR {
                    break n;
                }
            };
            if count != 1 || command != b'X' {
                return 3;
            }
            let count = loop {
                let n = libc::read(0, (&mut command as *mut u8).cast(), 1);
                if n >= 0 || *libc::__errno_location() != libc::EINTR {
                    break n;
                }
            };
            if count != 0 {
                return 4;
            }
            libc::puts(c"read-ok".as_ptr());
            return 0;
        }
        if lifecycle {
            let fd = channel_descriptor();
            let child_argv = [
                *argv as *const c_char,
                c"--child-exec-check".as_ptr(),
                ptr::null(),
            ];
            let vfork_argv = [
                *argv as *const c_char,
                c"--vfork-exec-check".as_ptr(),
                ptr::null(),
            ];
            let exec_argv = [
                *argv as *const c_char,
                c"--after-exec".as_ptr(),
                ptr::null(),
            ];
            while libc::read(0, (&mut command as *mut u8).cast(), 1) == 1 {
                if command == b'E' {
                    libc::execve(
                        c"/proc/self/exe".as_ptr(),
                        exec_argv.as_ptr(),
                        environ.cast(),
                    );
                    return 6;
                }
                if command == b'C' {
                    libc::close(fd);
                    if libc::dup2(1, fd) != fd {
                        return 7;
                    }
                    libc::puts(c"channel-closed".as_ptr());
                    libc::fflush(ptr::null_mut());
                    continue;
                }
                if !b"FXRV".contains(&command) {
                    return 8;
                }
                let child = if command == b'V' {
                    vfork_exec(
                        c"/proc/self/exe".as_ptr(),
                        vfork_argv.as_ptr(),
                        environ.cast(),
                    )
                } else {
                    libc::fork()
                };
                if child < 0 {
                    return 9;
                }
                if child == 0 {
                    if command == b'R' {
                        libc::_exit(if libc::fcntl(fd, libc::F_GETFD) < 0 {
                            10
                        } else {
                            0
                        });
                    }
                    if libc::fcntl(fd, libc::F_GETFD) >= 0
                        || *libc::__errno_location() != libc::EBADF
                    {
                        libc::_exit(11);
                    }
                    if command == b'X' {
                        if libc::dup2(1, fd) != fd {
                            libc::_exit(12);
                        }
                        libc::execve(
                            c"/proc/self/exe".as_ptr(),
                            child_argv.as_ptr(),
                            environ.cast(),
                        );
                        libc::_exit(13);
                    }
                    libc::_exit(0);
                }
                let mut status = 0;
                if libc::waitpid(child, &mut status, 0) != child
                    || !libc::WIFEXITED(status)
                    || libc::WEXITSTATUS(status) != 0
                {
                    return 14;
                }
                libc::puts(c"fork-parent-ok".as_ptr());
                libc::fflush(ptr::null_mut());
            }
            return 0;
        }
        loop {
            for i in 0..4096 {
                if live.add(i).read_volatile() != 0xa5 {
                    return 1;
                }
            }
            if !busy {
                libc::usleep(10000);
            }
        }
    }
}

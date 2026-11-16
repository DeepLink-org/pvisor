//! B-ALLOCATOR-RECLAIM-ENG: glibc retained free heap with four allocation threads.
#![no_std]
#![no_main]
mod fixture_runtime;
use core::{
    ffi::{CStr, c_char, c_void},
    mem::MaybeUninit,
    ptr,
};

struct Allocation {
    memory: *mut u8,
    size: usize,
    seed: usize,
}
extern "C" fn allocate(arg: *mut c_void) -> *mut c_void {
    // SAFETY: fill gives each thread exclusive access to one block until pthread_join.
    unsafe {
        let block = &mut *arg.cast::<Allocation>();
        block.memory = libc::malloc(block.size).cast();
        if block.memory.is_null() {
            libc::abort();
        }
        for i in 0..block.size {
            block.memory.add(i).write_volatile(pattern(i, block.seed));
        }
    }
    ptr::null_mut()
}
fn pattern(i: usize, seed: usize) -> u8 {
    (1 + ((i * 17 + i / 4096 + seed) % 251)) as u8
}
fn verify(block: &Allocation) {
    // SAFETY: caller holds a joined, live allocation of exactly block.size bytes.
    unsafe {
        for i in 0..block.size {
            if block.memory.add(i).read_volatile() != pattern(i, block.seed) {
                libc::abort();
            }
        }
    }
}
fn fill(blocks: &mut [Allocation; 4], bytes: usize) {
    let mut threads = [MaybeUninit::<libc::pthread_t>::uninit(); 4];
    for (i, block) in blocks.iter_mut().enumerate() {
        block.size = bytes / 4;
        block.seed = 37 * i;
        // SAFETY: the block stays at a stable address, exclusively owned by its worker until join.
        if unsafe {
            libc::pthread_create(
                threads[i].as_mut_ptr(),
                ptr::null(),
                allocate,
                (block as *mut Allocation).cast(),
            )
        } != 0
        {
            // SAFETY: abort ends the fixture before any borrowed block can leave scope.
            unsafe {
                libc::abort();
            }
        }
    }
    for thread in threads {
        // SAFETY: all four pthread_create calls succeeded; each is joined once before inspection.
        unsafe {
            if libc::pthread_join(thread.assume_init(), ptr::null_mut()) != 0 {
                libc::abort();
            }
        }
    }
    for block in blocks {
        verify(block);
    }
}
fn marker(phase: &CStr) {
    // SAFETY: phase is NUL-terminated; format types match and fflush flushes owned libc streams.
    unsafe {
        libc::printf(
            c"{\"phase\":\"%s\",\"pid\":%ld}\n".as_ptr(),
            phase.as_ptr(),
            i64::from(libc::getpid()),
        );
        libc::fflush(ptr::null_mut());
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn fixture_main(argc: i32, argv: *mut *mut c_char) -> i32 {
    // SAFETY: glibc supplies argv; all raw accesses stay within live allocations.
    unsafe {
        if argc != 2 {
            return 2;
        }
        let argument = CStr::from_ptr(*argv.add(1)).to_bytes();
        if argument == b"--idle" {
            loop {
                libc::pause();
            }
        }
        let mib = match argument {
            b"64" => 64,
            b"256" => 256,
            _ => return 2,
        };
        if libc::mallopt(libc::M_TRIM_THRESHOLD, -1) == 0
            || libc::mallopt(libc::M_MMAP_MAX, 0) == 0
            || libc::mallopt(libc::M_ARENA_MAX, 1) == 0
        {
            return 3;
        }
        let bytes = mib * 1024 * 1024;
        let mut blocks = core::array::from_fn(|_| Allocation {
            memory: ptr::null_mut(),
            size: 0,
            seed: 0,
        });
        fill(&mut blocks, bytes);
        let guard = libc::malloc(65536).cast::<u8>();
        if guard.is_null() {
            libc::abort();
        }
        for i in 0..65536 {
            guard.add(i).write_volatile(0xa5);
        }
        marker(c"allocated");
        let mut command = 0u8;
        loop {
            let count = libc::read(0, (&mut command as *mut u8).cast(), 1);
            if count < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            if count != 1 {
                return 4;
            }
            for i in 0..65536 {
                if guard.add(i).read_volatile() != 0xa5 {
                    libc::abort();
                }
            }
            match command {
                b'F' => {
                    for block in &mut blocks {
                        verify(block);
                        libc::free(block.memory.cast());
                        block.memory = ptr::null_mut();
                    }
                    marker(c"freed");
                }
                b'R' => {
                    fill(&mut blocks, bytes);
                    marker(c"refilled-verified");
                }
                b'Q' => {
                    for block in &blocks {
                        verify(block);
                    }
                    marker(c"verified-exit");
                    return 0;
                }
                _ => return 5,
            }
        }
    }
}

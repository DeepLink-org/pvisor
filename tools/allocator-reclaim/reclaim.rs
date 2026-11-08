//! Cooperative glibc trim worker, loaded into the target process with LD_PRELOAD.
#![no_std]

use core::{
    ffi::c_void,
    mem::MaybeUninit,
    ptr,
    sync::atomic::{AtomicI32, AtomicU64, Ordering},
};

const MAGIC: u32 = 0x50565231;
static CHANNEL: AtomicI32 = AtomicI32::new(-1);
static INODE: AtomicU64 = AtomicU64::new(0);

// Dependencies are supplied by the selected guest glibc, never a bundled allocator.
#[link(name = "libc.so.6", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn __register_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
        dso: *mut c_void,
    ) -> i32;
}
#[link(name = "libpthread.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {}
#[link(name = "libdl.so.2", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: abort takes no pointers and cannot unwind across the preload ABI.
    unsafe { libc::abort() }
}

fn descriptor_inode(fd: i32) -> Option<u64> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the kernel fills the native Linux stat layout; no glibc fstat version is required.
    if unsafe { libc::syscall(libc::SYS_fstat, fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: successful fstat initialized the entire stat buffer.
    let stat = unsafe { stat.assume_init() };
    (stat.st_mode & libc::S_IFMT == libc::S_IFSOCK).then_some(stat.st_ino)
}

fn valid_channel() -> bool {
    descriptor_inode(CHANNEL.load(Ordering::Relaxed)) == Some(INODE.load(Ordering::Relaxed))
}

unsafe extern "C" fn fork_child() {
    if valid_channel() {
        // SAFETY: only close the inherited descriptor while its socket identity still matches.
        unsafe {
            libc::close(CHANNEL.load(Ordering::Relaxed));
        }
    }
    CHANNEL.store(-1, Ordering::Relaxed);
}

fn load32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

extern "C" fn worker(_: *mut c_void) -> *mut c_void {
    // SAFETY: fixed NUL-terminated symbols are looked up in the target's global loader scope.
    let (allocate, glibc_allocate, trim, version) = unsafe {
        (
            libc::dlsym(libc::RTLD_DEFAULT, c"malloc".as_ptr()),
            libc::dlsym(libc::RTLD_DEFAULT, c"__libc_malloc".as_ptr()),
            libc::dlsym(libc::RTLD_DEFAULT, c"malloc_trim".as_ptr()),
            libc::dlsym(libc::RTLD_DEFAULT, c"gnu_get_libc_version".as_ptr()),
        )
    };
    let supported =
        !allocate.is_null() && allocate == glibc_allocate && !trim.is_null() && !version.is_null();
    let fd = CHANNEL.load(Ordering::Relaxed);
    loop {
        let mut request = [0u8; 16];
        // SAFETY: request is a writable buffer; recv never writes beyond its length.
        let size = unsafe {
            libc::recv(
                fd,
                request.as_mut_ptr().cast(),
                request.len(),
                libc::MSG_TRUNC,
            )
        };
        if size < 0 && interrupted() {
            continue;
        }
        if !valid_channel() || size != 16 || load32(&request) != MAGIC || load32(&request[4..]) > 1
        {
            break;
        }
        let result = if supported && load32(&request[4..]) == 1 {
            // SAFETY: the non-null symbol has glibc malloc_trim's C signature; binding was checked above.
            let trim: unsafe extern "C" fn(usize) -> i32 = unsafe { core::mem::transmute(trim) };
            unsafe { trim(0) }
        } else {
            0
        };
        let mut response = [0u8; 32];
        // SAFETY: getpid/gettid have no pointer arguments.
        let fields = unsafe {
            [
                MAGIC,
                u32::from(!supported),
                libc::getpid() as u32,
                libc::syscall(libc::SYS_gettid) as u32,
            ]
        };
        for (chunk, value) in response[..16].as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        response[16..24].copy_from_slice(&request[8..16]);
        response[24..28].copy_from_slice(&result.to_le_bytes());
        response[28..32].copy_from_slice(&u32::from(supported).to_le_bytes());
        if !valid_channel() {
            break;
        }
        let sent = loop {
            // SAFETY: response is readable for the complete packet; SIGPIPE is suppressed.
            let size = unsafe {
                libc::send(
                    fd,
                    response.as_ptr().cast(),
                    response.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if size >= 0 || !interrupted() {
                break size;
            }
        };
        if sent != 32 {
            break;
        }
    }
    // Keep the descriptor reserved until exec/exit, including after worker failure.
    ptr::null_mut()
}

fn interrupted() -> bool {
    // SAFETY: glibc returns this thread's valid errno pointer.
    unsafe { *libc::__errno_location() == libc::EINTR }
}

unsafe extern "C" fn initialize() {
    // SAFETY: getenv returns a NUL-terminated string, used only during this constructor.
    let value = unsafe { libc::getenv(c"PVISOR_RECLAIM_CHANNEL".as_ptr()) };
    if value.is_null() {
        return;
    }
    let (mut fd, mut inode, mut owner, mut trailing) = (0i32, 0u64, 0i64, 0u8);
    // SAFETY: format matches the writable output types, including the optional trailing byte.
    if unsafe {
        libc::sscanf(
            value,
            c"%d:%llu:%ld%c".as_ptr(),
            &mut fd,
            &mut inode,
            &mut owner,
            &mut trailing,
        )
    } != 3
        || fd < 3
        || owner < 0
        || descriptor_inode(fd) != Some(inode)
    {
        return;
    }
    let mut kind = 0i32;
    let mut length = core::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: kind and length are valid socket-option buffers.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut i32).cast(),
            &mut length,
        )
    } != 0
        || kind != libc::SOCK_SEQPACKET
    {
        return;
    }
    // SAFETY: getpid has no pointer arguments.
    let pid = unsafe { libc::getpid() };
    if owner != 0 && owner != i64::from(pid) {
        // SAFETY: socket identity was verified before closing this inherited channel.
        unsafe {
            libc::close(fd);
        }
        return;
    }
    let mut binding = [0u8; 96];
    // SAFETY: bounded output and format match fd/inode/pid; setenv copies the resulting string.
    let size = unsafe {
        libc::snprintf(
            binding.as_mut_ptr().cast(),
            binding.len(),
            c"%d:%llu:%ld".as_ptr(),
            fd,
            inode,
            i64::from(pid),
        )
    };
    if size < 0
        || size as usize >= binding.len()
        || unsafe {
            libc::setenv(
                c"PVISOR_RECLAIM_CHANNEL".as_ptr(),
                binding.as_ptr().cast(),
                1,
            )
        } != 0
    {
        return;
    }
    CHANNEL.store(fd, Ordering::Relaxed);
    INODE.store(inode, Ordering::Relaxed);
    // glibc's pthread_atfork wrapper uses this stable registration ABI. The preload
    // lives until process exit, so its callback requires no dlclose unregister owner.
    // SAFETY: callback uses only async-signal-safe calls and lock-free atomics after fork.
    if unsafe { __register_atfork(None, None, Some(fork_child), ptr::null_mut()) } != 0 {
        return;
    }
    let mut attributes = MaybeUninit::<libc::pthread_attr_t>::uninit();
    // SAFETY: pthread_attr_init initializes the correctly sized/aligned opaque object.
    if unsafe { libc::pthread_attr_init(attributes.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: attributes was initialized; masks/thread receive call-local output and are used only on success.
    unsafe {
        let attributes = attributes.as_mut_ptr();
        if libc::pthread_attr_setdetachstate(attributes, libc::PTHREAD_CREATE_DETACHED) == 0
            && libc::pthread_attr_setstacksize(attributes, 256 * 1024) == 0
        {
            let mut blocked = MaybeUninit::<libc::sigset_t>::uninit();
            let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigfillset(blocked.as_mut_ptr());
            if libc::pthread_sigmask(libc::SIG_SETMASK, blocked.as_ptr(), previous.as_mut_ptr())
                == 0
            {
                let mut thread = MaybeUninit::<libc::pthread_t>::uninit();
                libc::pthread_create(thread.as_mut_ptr(), attributes, worker, ptr::null_mut());
                libc::pthread_sigmask(libc::SIG_SETMASK, previous.as_ptr(), ptr::null_mut());
            }
        }
        libc::pthread_attr_destroy(attributes);
    }
}

#[used]
#[unsafe(link_section = ".init_array")]
static INITIALIZE: unsafe extern "C" fn() = initialize;

//! Standalone Linux/musl guest payload; compile for the VM's architecture.
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct StatxTimestamp {
    seconds: i64,
    nanoseconds: u32,
    reserved: i32,
}

// The Linux statx ABI has the same 256-byte layout on x86_64 and aarch64.
// Keep the fixed timestamp prefix; remaining device/extension fields are unused.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct StatxRecord {
    mask: u32,
    block_size: u32,
    attributes: u64,
    links: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    reserved: u16,
    inode: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    atime: StatxTimestamp,
    btime: StatxTimestamp,
    ctime: StatxTimestamp,
    mtime: StatxTimestamp,
    unused: [u64; 16],
}

unsafe extern "C" {
    fn syscall(number: std::ffi::c_long, ...) -> std::ffi::c_long;
    fn ioctl(fd: i32, command: usize, ...) -> i32;
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn umount(target: *const std::ffi::c_char) -> i32;
    fn utimensat(
        dirfd: i32,
        path: *const std::ffi::c_char,
        times: *const std::ffi::c_void,
        flags: i32,
    ) -> i32;
    fn setxattr(
        path: *const std::ffi::c_char,
        name: *const std::ffi::c_char,
        value: *const std::ffi::c_void,
        size: usize,
        flags: i32,
    ) -> i32;
    fn removexattr(path: *const std::ffi::c_char, name: *const std::ffi::c_char) -> i32;
    fn close(fd: i32) -> i32;
    fn flock(fd: i32, operation: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
    fn msync(address: *mut std::ffi::c_void, length: usize, flags: i32) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(status: i32) -> !;
    fn pipe2(descriptors: *mut i32, flags: i32) -> i32;
    fn memfd_create(name: *const std::ffi::c_char, flags: u32) -> i32;
    fn sendfile(output: i32, input: i32, offset: *mut i64, count: usize) -> isize;
    fn splice(
        input: i32,
        input_offset: *mut i64,
        output: i32,
        output_offset: *mut i64,
        count: usize,
        flags: u32,
    ) -> isize;
    fn copy_file_range(
        input: i32,
        input_offset: *mut i64,
        output: i32,
        output_offset: *mut i64,
        count: usize,
        flags: u32,
    ) -> isize;
}

const STATX_ATIME: u32 = 0x20;
const STATX_MTIME: u32 = 0x40;
const STATX_INO: u32 = 0x100;
const STATX_SIZE: u32 = 0x200;
const STATX_BASIC_STATS: u32 = 0x7ff;
const AT_STATX_FORCE_SYNC: usize = 0x2000;

fn statx_record(path: &std::ffi::CStr, mask: u32, flags: usize) -> StatxRecord {
    statx_record_at(-100, path, mask, flags)
}

fn statx_record_at(dirfd: isize, path: &std::ffi::CStr, mask: u32, flags: usize) -> StatxRecord {
    let number = if cfg!(target_arch = "x86_64") {
        332
    } else if cfg!(target_arch = "aarch64") {
        291
    } else {
        panic!("statx diagnostic supports x86_64 and aarch64")
    };
    let mut record = StatxRecord::default();
    assert_eq!(std::mem::size_of::<StatxRecord>(), 256);
    // Use the exact requested mask, bypassing libc's stat/fstat emulation.
    assert_eq!(
        unsafe {
            syscall(
                number,
                dirfd,
                path.as_ptr(),
                flags,
                mask as usize,
                &mut record as *mut StatxRecord,
            )
        },
        0,
        "statx({path:?}, mask={mask:#x}, flags={flags:#x}): {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(record.mask & mask, mask, "requested statx fields missing");
    record
}

struct AtimePhase {
    before: u64,
    after: u64,
    syscall_ns: u128,
}

impl AtimePhase {
    fn json(&self) -> String {
        format!(
            "{{\"tokens\":[{},{}],\"syscall_ns\":{}}}",
            self.before, self.after, self.syscall_ns
        )
    }
}

fn atime_checkpoint(directory: &File, token: u64) {
    let bytes = token.to_le_bytes();
    assert_eq!(
        unsafe { ioctl(directory.as_raw_fd(), 0x4008_76f1, bytes.as_ptr()) },
        0,
        "atime requires PVISOR_FS_PROFILE=1 and the synchronous phase ioctl: {}",
        std::io::Error::last_os_error()
    );
}

fn atime_phase<T>(
    directory: &File,
    token: &mut u64,
    action: impl FnOnce() -> T,
) -> (T, AtimePhase) {
    *token += 1;
    let before = *token;
    atime_checkpoint(directory, before);
    let started = Instant::now();
    let value = action();
    let syscall_ns = started.elapsed().as_nanos();
    *token += 1;
    atime_checkpoint(directory, *token);
    (
        value,
        AtimePhase {
            before,
            after: *token,
            syscall_ns,
        },
    )
}

fn atime_stat_json(record: &StatxRecord) -> String {
    format!(
        "{{\"mask\":{},\"inode\":{},\"size\":{},\"atime\":[{},{}],\"mtime\":[{},{}]}}",
        record.mask,
        record.inode,
        record.size,
        record.atime.seconds,
        record.atime.nanoseconds,
        record.mtime.seconds,
        record.mtime.nanoseconds,
    )
}

fn check_atime() {
    mount_share("metadata-stable", "/tmp/metadata-stable");
    mount_share("metadata-mutable", "/tmp/metadata-mutable");
    let mut token = 0;
    let mut cases = 0;
    for (location, scope, directory, prefix) in [
        ("root-lower", "rootfs", "/", "/atime-lower"),
        ("root-upper", "rootfs", "/", "/atime-upper"),
        (
            "stable-lower",
            "metadata-stable",
            "/tmp/metadata-stable",
            "/tmp/metadata-stable/atime",
        ),
        (
            "mutable-lower",
            "metadata-mutable",
            "/tmp/metadata-mutable",
            "/tmp/metadata-mutable/atime",
        ),
    ] {
        let directory = File::open(directory).unwrap();
        for (mask_name, request_mask) in [
            ("size-mtime", STATX_SIZE | STATX_MTIME),
            ("atime", STATX_ATIME),
            ("basic", STATX_BASIC_STATS),
        ] {
            let path = std::ffi::CString::new(format!("{prefix}-{mask_name}")).unwrap();
            let mut reader = File::open(path.to_str().unwrap()).unwrap();
            for access in ["no-read", "first-read", "cached-read"] {
                reader.seek(SeekFrom::Start(0)).unwrap();
                let ((primed, primed_at), prime_phase) =
                    atime_phase(&directory, &mut token, || {
                        let primed = statx_record(&path, STATX_BASIC_STATS, AT_STATX_FORCE_SYNC);
                        (primed, Instant::now())
                    });
                assert_eq!(primed.size, 8);
                let ((), read_phase) = atime_phase(&directory, &mut token, || {
                    if access != "no-read" {
                        let mut bytes = [0u8; 8];
                        reader.read_exact(&mut bytes).unwrap();
                        assert_eq!(&bytes, b"original");
                    }
                });
                let ((first, prime_to_first_ns), first_phase) =
                    atime_phase(&directory, &mut token, || {
                        let age = primed_at.elapsed().as_nanos();
                        (statx_record(&path, request_mask, 0), age) // AT_STATX_SYNC_AS_STAT
                    });
                let (repeat, repeat_phase) = atime_phase(&directory, &mut token, || {
                    statx_record(&path, request_mask, 0)
                });
                for record in [&first, &repeat] {
                    if request_mask & STATX_SIZE != 0 {
                        assert_eq!(record.size, primed.size);
                    }
                    if request_mask & STATX_MTIME != 0 {
                        assert_eq!(record.mtime.seconds, primed.mtime.seconds);
                        assert_eq!(record.mtime.nanoseconds, primed.mtime.nanoseconds);
                    }
                    if record.mask & STATX_INO != 0 {
                        assert_eq!(record.inode, primed.inode);
                    }
                }
                println!(
                    "ATIME_CASE {{\"location\":\"{location}\",\"scope\":\"{scope}\",\"access\":\"{access}\",\"mask_name\":\"{mask_name}\",\"request_mask\":{request_mask},\"prime_to_first_ns\":{prime_to_first_ns},\"prime\":{},\"read\":{},\"stat_first\":{},\"stat_repeat\":{},\"primed\":{},\"first\":{},\"repeat\":{}}}",
                    prime_phase.json(),
                    read_phase.json(),
                    first_phase.json(),
                    repeat_phase.json(),
                    atime_stat_json(&primed),
                    atime_stat_json(&first),
                    atime_stat_json(&repeat),
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 36);
    assert_eq!(token, 288);
    println!("ATIME_SUMMARY {{\"cases\":36,\"phases\":144,\"checkpoints\":288,\"sleeps\":0}}");
    assert_eq!(unsafe { umount(c"/tmp/metadata-stable".as_ptr()) }, 0);
    assert_eq!(unsafe { umount(c"/tmp/metadata-mutable".as_ptr()) }, 0);
    println!("GUEST_METADATA_PASS atime");
}

fn check_lifetime(mode: &str) {
    for kind in ["time", "setxattr", "removexattr", "link"] {
        if mode != "review" && mode != format!("review-{kind}") {
            continue;
        }
        let name = format!("/lifetime-{kind}");
        let path = std::ffi::CString::new(name.as_bytes()).unwrap();
        let mut held = File::open(&name).unwrap();
        // No content access before the metadata-only copy-up. The descriptor
        // must retain its original physical lower object across the later write.
        match kind {
            "time" => assert_eq!(
                unsafe { utimensat(-100, path.as_ptr(), std::ptr::null(), 0) },
                0
            ),
            "setxattr" => assert_eq!(
                unsafe {
                    setxattr(
                        path.as_ptr(),
                        c"user.lifetime".as_ptr(),
                        b"v".as_ptr().cast(),
                        1,
                        0,
                    )
                },
                0
            ),
            "removexattr" => assert_eq!(
                unsafe { removexattr(path.as_ptr(), c"user.lifetime".as_ptr()) },
                0
            ),
            "link" => fs::hard_link(&name, "/lifetime-alias").unwrap(),
            _ => unreachable!(),
        }
        fs::write(&name, b"modified").unwrap();
        let mut original = Vec::new();
        held.read_to_end(&mut original).unwrap();
        assert_eq!(
            original, b"original",
            "{kind} copy-up rebound an existing descriptor"
        );
        assert_eq!(fs::read(&name).unwrap(), b"modified");
        println!("LIFETIME_PASS {kind}");
    }
}

fn mount_share(tag: &str, path: &str) {
    fs::create_dir_all(path).unwrap();
    let tag = std::ffi::CString::new(tag).unwrap();
    let path = std::ffi::CString::new(path).unwrap();
    // The fixture only mounts its own virtio-fs attachments inside the guest.
    assert_eq!(
        unsafe {
            mount(
                tag.as_ptr(),
                path.as_ptr(),
                c"virtiofs".as_ptr(),
                0,
                std::ptr::null(),
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

fn json_string(value: &str) -> String {
    let mut result = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => result.push_str(&format!("\\u{:04x}", c as u32)),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

// This unknown ioctl reaches the normal guest bind path without reading a page.
// ENOTTY is the expected host answer; no new diagnostic host API is required.
fn bind_without_read(file: &File) {
    let result = unsafe { ioctl(file.as_raw_fd(), 0x76f3usize, 0usize) };
    assert_eq!(result, -1, "unknown ioctl unexpectedly succeeded");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(25));
}

fn pread_bytes(file: &File, size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0).unwrap();
    let mut extra = [0];
    assert_eq!(file.read_at(&mut extra, size as u64).unwrap(), 0);
    bytes
}

fn compare_bytes(errors: &mut Vec<String>, label: &str, got: &[u8], expected: &[u8]) {
    if got != expected {
        let mismatch = got.iter().zip(expected).position(|(a, b)| a != b);
        errors.push(format!(
            "{label}: first mismatch {mismatch:?}, lengths {}/{}",
            got.len(),
            expected.len()
        ));
    }
}

fn review_case(kind: &str, name: &str, action: impl FnOnce() -> Vec<String>) -> bool {
    // Each fixture is independent: one failed contract must not hide other cases.
    let errors =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("case panicked");
            vec![message.to_owned()]
        });
    let passed = errors.is_empty();
    println!(
        "{kind}_CASE {{\"name\":{},\"status\":\"{}\",\"errors\":[{}]}}",
        json_string(name),
        if passed { "pass" } else { "failure" },
        errors
            .iter()
            .map(|error| json_string(error))
            .collect::<Vec<_>>()
            .join(",")
    );
    passed
}

fn review_summary(mode: &str, kind: &str, cases: usize, failed: usize) {
    println!("{kind}_SUMMARY {{\"cases\":{cases},\"failed\":{failed}}}");
    if failed != 0 {
        std::process::exit(1);
    }
    println!("GUEST_METADATA_PASS {mode}");
}

fn close_checked(file: File) {
    // File::drop deliberately ignores close errors; this probe must observe them.
    assert_eq!(
        unsafe { close(file.into_raw_fd()) },
        0,
        "close: {}",
        std::io::Error::last_os_error()
    );
}

fn check_review_flush() {
    mount_share("metadata-stable", "/tmp/metadata-stable");
    mount_share("metadata-mutable", "/tmp/metadata-mutable");
    let mut failed = 0;
    let mut cases = 0;
    for (name, path) in [
        ("root-lower", "/flush-read"),
        ("stable-lower", "/tmp/metadata-stable/flush-read"),
        ("mutable-lower", "/tmp/metadata-mutable/flush-read"),
        ("root-upper", "/flush-upper"),
    ] {
        for bound in [false, true] {
            cases += 1;
            let passed = review_case("FLUSH", &format!("{name}-bound-{bound}"), || {
                let file = File::open(path).unwrap();
                if bound {
                    let mut byte = [0];
                    assert_eq!(file.read_at(&mut byte, 0).unwrap(), 1);
                    assert_eq!(byte[0], if name == "root-upper" { b'u' } else { b'o' });
                }
                let duplicate = file.try_clone().unwrap();
                close_checked(file);
                if bound {
                    let mut byte = [0];
                    assert_eq!(duplicate.read_at(&mut byte, 0).unwrap(), 1);
                }
                close_checked(duplicate);
                Vec::new()
            });
            failed += usize::from(!passed);
        }
    }
    cases += 1;
    failed += usize::from(!review_case(
        "FLUSH",
        "lower-close-with-upper-writer",
        || {
            let lower = File::open("/flush-copyup").unwrap();
            // Bind the old object without warming its data pages, then keep the
            // upper writer open while closing the lower description.
            assert_eq!(unsafe { ioctl(lower.as_raw_fd(), 0x76f3, 0usize) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(25));
            let upper = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/flush-copyup")
                .unwrap();
            let changed = vec![b'b'; 8192];
            upper.write_all_at(&changed, 0).unwrap();
            close_checked(lower);
            assert_eq!(&pread_bytes(&upper, 16384)[..8192], &changed);
            upper.sync_all().unwrap();
            close_checked(upper);
            // This case checks closing/synchronization, not the independently
            // failing old-lower full-page read oracle in review-pages.
            Vec::new()
        },
    ));
    cases += 1;
    failed += usize::from(!review_case(
        "FLUSH",
        "local-flock-duplicate-lifetime",
        || {
            let file = File::open("/flush-read").unwrap();
            assert_eq!(unsafe { flock(file.as_raw_fd(), 2 | 4) }, 0);
            let duplicate = file.try_clone().unwrap();
            close_checked(file);
            let contender = File::open("/flush-read").unwrap();
            assert_eq!(unsafe { flock(contender.as_raw_fd(), 2 | 4) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(11));
            close_checked(duplicate);
            assert_eq!(unsafe { flock(contender.as_raw_fd(), 2 | 4) }, 0);
            close_checked(contender);
            Vec::new()
        },
    ));
    cases += 1;
    failed += usize::from(!review_case("FLUSH", "mmap-survives-close", || {
        let file = File::open("/flush-read").unwrap();
        let address = unsafe { mmap(std::ptr::null_mut(), 4096, 1, 2, file.as_raw_fd(), 0) };
        assert_ne!(
            address as isize,
            -1,
            "mmap: {}",
            std::io::Error::last_os_error()
        );
        close_checked(file);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(address.cast::<u8>(), 8) },
            b"original"
        );
        assert_eq!(unsafe { munmap(address, 4096) }, 0);
        Vec::new()
    }));
    assert_eq!(unsafe { umount(c"/tmp/metadata-stable".as_ptr()) }, 0);
    assert_eq!(unsafe { umount(c"/tmp/metadata-mutable".as_ptr()) }, 0);
    review_summary("review-flush", "FLUSH", cases, failed);
}

fn check_review_pages() {
    let original: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let mut failed = 0;
    for bound in [false, true] {
        for size in [4096, 8192] {
            let name = format!("root-lower-bound-{bound}-cold-write-{size}-offset-0");
            let passed = review_case("PAGE", &name, || {
                let path = format!("/pages-{bound}-{size}");
                let held = File::open(&path).unwrap();
                if bound {
                    bind_without_read(&held);
                }
                // Keep this same writable descriptor for the upper reads. Opening a
                // new reader could invalidate the shared mapping and hide the bug.
                let upper = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                let patch = vec![0xd3; size];
                assert_eq!(upper.write_at(&patch, 0).unwrap(), size);
                let mut expected_upper = original.clone();
                expected_upper[..size].copy_from_slice(&patch);
                let mut errors = Vec::new();
                for round in 0..3 {
                    // The old lower FD is deliberately checked before the upper FD.
                    compare_bytes(
                        &mut errors,
                        &format!("lower round {round}"),
                        &pread_bytes(&held, original.len()),
                        &original,
                    );
                    compare_bytes(
                        &mut errors,
                        &format!("upper round {round}"),
                        &pread_bytes(&upper, original.len()),
                        &expected_upper,
                    );
                }
                errors
            });
            failed += usize::from(!passed);
        }
    }
    review_summary("review-pages", "PAGE", 4, failed);
}

fn attrs_json(record: &StatxRecord) -> String {
    format!("{{\"inode\":{},\"mode\":{},\"nlink\":{},\"uid\":{},\"gid\":{},\"size\":{},\"atime\":[{},{}],\"mtime\":[{},{}],\"ctime\":[{},{}]}}",
        record.inode, record.mode, record.links, record.uid, record.gid, record.size,
        record.atime.seconds, record.atime.nanoseconds, record.mtime.seconds,
        record.mtime.nanoseconds, record.ctime.seconds, record.ctime.nanoseconds)
}

fn compare_current_attrs(
    errors: &mut Vec<String>,
    label: &str,
    first: &StatxRecord,
    forced: &StatxRecord,
) {
    if attrs_json(first) != attrs_json(forced) {
        errors.push(format!(
            "{label}: ordinary {} != force-sync {}",
            attrs_json(first),
            attrs_json(forced)
        ));
    }
}

fn check_review_attrs() {
    let kinds = [
        "chmod",
        "time",
        "truncate",
        "write",
        "copyup",
        "link",
        "rename-overwrite",
        "unlink-recreate",
        "alias",
    ];
    let mut failed = 0;
    for kind in kinds {
        let passed = review_case("ATTR", kind, || {
            let name = format!("/attrs-{kind}");
            let path = std::ffi::CString::new(name.as_str()).unwrap();
            let held = File::open(&name).unwrap();
            bind_without_read(&held);
            let primed = statx_record(&path, STATX_BASIC_STATS, AT_STATX_FORCE_SYNC);
            let original_fd = held.metadata().unwrap();
            let mut errors = Vec::new();
            let expected_bytes: &[u8] = match kind {
                "chmod" => {
                    fs::set_permissions(&name, fs::Permissions::from_mode(0o600)).unwrap();
                    b"original"
                }
                "time" => {
                    #[repr(C)]
                    struct Timespec {
                        seconds: i64,
                        nanoseconds: i64,
                    }
                    let times = [
                        Timespec {
                            seconds: 1234567800,
                            nanoseconds: 123,
                        },
                        Timespec {
                            seconds: 1234567900,
                            nanoseconds: 456,
                        },
                    ];
                    assert_eq!(
                        unsafe { utimensat(-100, path.as_ptr(), times.as_ptr().cast(), 0) },
                        0
                    );
                    b"original"
                }
                "truncate" => {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&name)
                        .unwrap()
                        .set_len(3)
                        .unwrap();
                    b"ori"
                }
                "write" => {
                    fs::write(&name, b"modified").unwrap();
                    b"modified"
                }
                "copyup" => {
                    drop(fs::OpenOptions::new().write(true).open(&name).unwrap());
                    b"original"
                }
                "link" => {
                    fs::hard_link(&name, "/attrs-link-other").unwrap();
                    b"original"
                }
                "rename-overwrite" => {
                    fs::rename("/attrs-rename-source", &name).unwrap();
                    b"replacement"
                }
                "unlink-recreate" => {
                    fs::remove_file(&name).unwrap();
                    fs::write(&name, b"recreated").unwrap();
                    b"recreated"
                }
                "alias" => {
                    let alias = c"/attrs-alias-other";
                    let _ = statx_record(alias, STATX_BASIC_STATS, AT_STATX_FORCE_SYNC);
                    assert_eq!(primed.links, 2);
                    fs::set_permissions("/attrs-alias-other", fs::Permissions::from_mode(0o600))
                        .unwrap();
                    let first = statx_record(&path, STATX_BASIC_STATS, 0);
                    let forced = statx_record(&path, STATX_BASIC_STATS, AT_STATX_FORCE_SYNC);
                    compare_current_attrs(&mut errors, "alias chmod", &first, &forced);
                    if first.mode & 0o7777 != 0o600 {
                        errors.push("alias chmod not visible through original name".into());
                    }
                    if first.links != 2 {
                        errors.push(format!("alias chmod nlink {} != 2", first.links));
                    }
                    fs::remove_file("/attrs-alias-other").unwrap();
                    b"original"
                }
                _ => unreachable!(),
            };
            // Capture natural stat/fstat before using FORCE_SYNC as an oracle.
            let first = statx_record(&path, STATX_BASIC_STATS, 0);
            let old_fd = held.metadata().unwrap();
            let forced = statx_record(&path, STATX_BASIC_STATS, AT_STATX_FORCE_SYNC);
            compare_current_attrs(&mut errors, "current name", &first, &forced);
            if first.size != expected_bytes.len() as u64 {
                errors.push(format!("size {} != {}", first.size, expected_bytes.len()));
            }
            if matches!(kind, "chmod" | "alias") && first.mode & 0o7777 != 0o600 {
                errors.push("current mode != 0600".into());
            }
            if kind == "time" && (first.mtime.seconds, first.mtime.nanoseconds) != (1234567900, 456)
            {
                errors.push("mtime not updated".into());
            }
            let expected_links = if kind == "link" { 2 } else { 1 };
            if first.links != expected_links {
                errors.push(format!("nlink {} != {expected_links}", first.links));
            }
            // The lower object was pinned by the explicit bind before copy-up.
            for (field, old, initial) in [
                ("inode", old_fd.ino(), original_fd.ino()),
                ("mode", old_fd.mode() as u64, original_fd.mode() as u64),
                ("nlink", old_fd.nlink(), original_fd.nlink()),
                ("uid", old_fd.uid() as u64, original_fd.uid() as u64),
                ("gid", old_fd.gid() as u64, original_fd.gid() as u64),
                ("size", old_fd.size(), original_fd.size()),
                ("mtime", old_fd.mtime() as u64, original_fd.mtime() as u64),
                (
                    "mtime_ns",
                    old_fd.mtime_nsec() as u64,
                    original_fd.mtime_nsec() as u64,
                ),
                ("ctime", old_fd.ctime() as u64, original_fd.ctime() as u64),
                (
                    "ctime_ns",
                    old_fd.ctime_nsec() as u64,
                    original_fd.ctime_nsec() as u64,
                ),
            ] {
                if old != initial {
                    errors.push(format!("old lower fd {field} {old} != {initial}"));
                }
            }
            compare_bytes(
                &mut errors,
                "old lower content",
                &pread_bytes(&held, 8),
                b"original",
            );
            compare_bytes(
                &mut errors,
                "current content",
                &fs::read(&name).unwrap(),
                expected_bytes,
            );
            println!(
                "ATTR_OBSERVATION {{\"name\":{},\"primed\":{},\"ordinary\":{},\"forced\":{}}}",
                json_string(kind),
                attrs_json(&primed),
                attrs_json(&first),
                attrs_json(&forced)
            );
            errors
        });
        failed += usize::from(!passed);
    }
    let passed = review_case("ATTR", "link-opath", || {
        let source = "/attrs-link-opath";
        let held = fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x20_0000)
            .open(source)
            .unwrap();
        let primed = statx_record_at(
            held.as_raw_fd() as isize,
            c"",
            STATX_BASIC_STATS,
            0x1000 | AT_STATX_FORCE_SYNC,
        );
        assert_eq!(primed.links, 1);
        fs::hard_link(source, "/attrs-link-opath-other").unwrap();
        // Diagnostic only: outlive the ordinary 1s TTL without any path lookup
        // that could refresh the source inode before this same O_PATH check.
        std::thread::sleep(Duration::from_millis(1100));
        let first = statx_record_at(held.as_raw_fd() as isize, c"", STATX_BASIC_STATS, 0x1000);
        let forced = statx_record_at(
            held.as_raw_fd() as isize,
            c"",
            STATX_BASIC_STATS,
            0x1000 | AT_STATX_FORCE_SYNC,
        );
        let mut errors = Vec::new();
        compare_current_attrs(&mut errors, "link source O_PATH", &first, &forced);
        if first.links != 2 {
            errors.push(format!("link source O_PATH nlink {} != 2", first.links));
        }
        println!("ATTR_OBSERVATION {{\"name\":\"link-opath\",\"primed\":{},\"ordinary\":{},\"forced\":{}}}", attrs_json(&primed), attrs_json(&first), attrs_json(&forced));
        errors
    });
    failed += usize::from(!passed);
    review_summary("review-attrs", "ATTR", kinds.len() + 1, failed);
}

fn data_pattern(size: usize, seed: usize) -> Vec<u8> {
    (0..size)
        .map(|index| ((index * 17 + index / 4096 * 29 + seed) % 251) as u8)
        .collect()
}

fn data_wait(child: i32) -> i32 {
    let mut status = 0;
    loop {
        let result = unsafe { waitpid(child, &mut status, 0) };
        if result == child {
            return status;
        }
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(4) {
            continue;
        }
        panic!("waitpid: {}", std::io::Error::last_os_error());
    }
}

fn data_in_child(name: &str, action: impl FnOnce() -> Vec<String>) -> Vec<String> {
    // A wrong mmap fault must not prevent collecting the remaining cases.
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    let child = unsafe { fork() };
    assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
    if child == 0 {
        let passed = review_case("DATA_CHILD", name, action);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        unsafe { _exit(if passed { 0 } else { 1 }) }
    }
    let status = data_wait(child);
    if status == 0 {
        Vec::new()
    } else {
        vec![format!(
            "child wait status {status}, signal {}; see DATA_CHILD_CASE",
            status & 0x7f
        )]
    }
}

fn data_run(
    cases: &mut usize,
    failed: &mut usize,
    name: &str,
    action: impl FnOnce() -> Vec<String>,
) {
    *cases += 1;
    *failed += usize::from(!review_case("DATA", name, || data_in_child(name, action)));
}

fn data_check_file(errors: &mut Vec<String>, label: &str, file: &mut File, expected: &[u8]) {
    // Bounded pread retains short reads and EOF errors as evidence, rather than
    // panicking before the other object's oracle can run.
    let mut bytes = Vec::new();
    while bytes.len() <= expected.len() {
        let remaining = expected.len() + 1 - bytes.len();
        let mut chunk = vec![0; remaining.min(4096)];
        match file.read_at(&mut chunk, bytes.len() as u64) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) => {
                errors.push(format!("{label} pread: {error}"));
                break;
            }
        }
    }
    compare_bytes(errors, label, &bytes, expected);
    let mut extra = [0];
    match file.read_at(&mut extra, expected.len() as u64) {
        Ok(0) => {}
        other => errors.push(format!("{label} EOF at {}: {other:?}", expected.len())),
    }
    match file.seek(SeekFrom::End(0)) {
        Ok(size) if size == expected.len() as u64 => {}
        other => errors.push(format!(
            "{label} SEEK_END expected {}: {other:?}",
            expected.len()
        )),
    }
}

struct DataMap {
    address: *mut u8,
    length: usize,
}

impl DataMap {
    fn new(file: &File, length: usize, protection: i32, flags: i32) -> Self {
        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                length,
                protection,
                flags,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(
            address as isize,
            -1,
            "mmap: {}",
            std::io::Error::last_os_error()
        );
        Self {
            address: address.cast(),
            length,
        }
    }

    fn bytes(&self, start: usize, length: usize) -> Vec<u8> {
        assert!(start <= self.length && length <= self.length - start);
        (start..start + length)
            .map(|index| unsafe { self.address.add(index).read_volatile() })
            .collect()
    }

    fn write(&self, start: usize, bytes: &[u8]) {
        assert!(start <= self.length && bytes.len() <= self.length - start);
        for (index, byte) in bytes.iter().enumerate() {
            unsafe { self.address.add(start + index).write_volatile(*byte) };
        }
    }

    fn expect_sigbus(&self, offset: usize, errors: &mut Vec<String>, label: &str) {
        assert!(offset < self.length);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        let child = unsafe { fork() };
        assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
        if child == 0 {
            unsafe {
                self.address.add(offset).read_volatile();
                _exit(0)
            }
        }
        let status = data_wait(child);
        println!("DATA_FAULT {{\"name\":{},\"offset\":{offset},\"expected_signal\":7,\"wait_status\":{status},\"actual_signal\":{}}}", json_string(label), status & 0x7f);
        if status & 0x7f != 7 {
            errors.push(format!("{label}: expected SIGBUS, wait status {status}"));
        }
    }
}

impl Drop for DataMap {
    fn drop(&mut self) {
        let _ = unsafe { munmap(self.address.cast(), self.length) };
    }
}

fn data_checkpoint(directory: &File, token: u64) {
    let bytes = token.to_le_bytes();
    assert_eq!(
        unsafe { ioctl(directory.as_raw_fd(), 0x4008_76f1, bytes.as_ptr()) },
        0,
        "review-data requires PVISOR_FS_PROFILE=1 and synchronous checkpoints: {}",
        std::io::Error::last_os_error()
    );
}

fn data_splice_bytes(source: &File, limit: usize) -> Result<Vec<u8>, String> {
    let mut descriptors = [-1; 2];
    if unsafe { pipe2(descriptors.as_mut_ptr(), 0x80000) } != 0 {
        return Err(format!("pipe2: {}", std::io::Error::last_os_error()));
    }
    let mut reader = unsafe { File::from_raw_fd(descriptors[0]) };
    let writer = unsafe { File::from_raw_fd(descriptors[1]) };
    let mut bytes = Vec::new();
    let mut position = 0i64;
    // Bound the number of iterations even if a broken splice does not advance.
    for _ in 0..limit / 4096 + 3 {
        let result = unsafe {
            splice(
                source.as_raw_fd(),
                &mut position,
                writer.as_raw_fd(),
                std::ptr::null_mut(),
                4096,
                0,
            )
        };
        if result < 0 {
            return Err(format!("splice: {}", std::io::Error::last_os_error()));
        }
        if result == 0 {
            return Ok(bytes);
        }
        let mut chunk = vec![0; result as usize];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| format!("pipe read: {error}"))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() > limit {
            return Ok(bytes);
        }
    }
    Err("splice did not reach EOF within bounded iterations".into())
}

fn data_copy_range_observation() {
    // generic_copy_file_range checks file_inode(source)->i_size before FUSE's
    // source read path. That known namespace-size boundary is observation only.
    let action = std::panic::catch_unwind(|| {
        let path = "/tmp/metadata-stable/data-copy-range";
        let source = File::open(path).unwrap();
        bind_without_read(&source);
        let upper = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        upper.set_len(4096 + 9).unwrap();
        let output = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open("/tmp/metadata-stable/data-copy-range-output")
            .unwrap();
        let (mut input_offset, mut output_offset) = (0i64, 0i64);
        let result = unsafe {
            copy_file_range(
                source.as_raw_fd(),
                &mut input_offset,
                output.as_raw_fd(),
                &mut output_offset,
                16384,
                0,
            )
        };
        let errno = if result < 0 {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        let output_bytes = fs::read("/tmp/metadata-stable/data-copy-range-output").unwrap();
        let original = data_pattern(16384, 3);
        println!("DATA_OBSERVATION {{\"name\":\"copy-file-range-namespace-size\",\"hard_case\":false,\"requested\":16384,\"current_namespace_size\":4105,\"returned\":{result},\"errno\":{},\"source_offset\":{input_offset},\"destination_offset\":{output_offset},\"bytes\":{},\"matches_original_prefix\":{},\"complete_original\":{}}}",
            errno.map_or_else(|| "null".into(), |value| value.to_string()), output_bytes.len(),
            original.starts_with(&output_bytes), output_bytes == original);
    });
    if action.is_err() {
        println!("DATA_OBSERVATION {{\"name\":\"copy-file-range-namespace-size\",\"hard_case\":false,\"status\":\"setup-error\"}}");
    }
}

fn check_review_data() {
    mount_share("metadata-stable", "/tmp/metadata-stable");
    mount_share("metadata-mutable", "/tmp/metadata-mutable");
    let (mut cases, mut failed) = (0usize, 0usize);
    for bound in [false, true] {
        for mutation in ["full", "partial", "zero", "shrink", "extend"] {
            let name = format!("lower-{mutation}-bound-{bound}");
            data_run(&mut cases, &mut failed, &name, || {
                let path = format!("/data-{mutation}-{bound}");
                let original = data_pattern(16384, 3);
                let mut lower = File::open(&path).unwrap();
                if bound {
                    bind_without_read(&lower);
                }
                let mut upper = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                let mut expected = original.clone();
                match mutation {
                    "full" => {
                        let patch = data_pattern(8192, 97);
                        upper.write_all_at(&patch, 0).unwrap();
                        expected[..8192].copy_from_slice(&patch);
                    }
                    "partial" => {
                        let patch = data_pattern(8, 97);
                        upper.write_all_at(&patch, 4093).unwrap();
                        expected[4093..4101].copy_from_slice(&patch);
                    }
                    "zero" => {
                        upper.set_len(0).unwrap();
                        expected.clear();
                    }
                    "shrink" => {
                        upper.set_len(4096 + 9).unwrap();
                        expected.truncate(4096 + 9);
                    }
                    "extend" => {
                        upper.set_len(24576).unwrap();
                        expected.resize(24576, 0);
                        let patch = data_pattern(4096, 97);
                        upper.write_all_at(&patch, 20480).unwrap();
                        expected[20480..].copy_from_slice(&patch);
                    }
                    _ => unreachable!(),
                }
                let mut errors = Vec::new();
                data_check_file(&mut errors, "old lower first", &mut lower, &original);
                data_check_file(&mut errors, "upper", &mut upper, &expected);
                data_check_file(&mut errors, "old lower repeat", &mut lower, &original);
                close_checked(lower);
                close_checked(upper);
                errors
            });
        }
    }

    data_run(&mut cases, &mut failed, "cross-fd-reopen", || {
        let directory = File::open("/tmp/metadata-stable").unwrap();
        let expected = data_pattern(16384, 3);
        let mut errors = Vec::new();
        for round in 1u64..=3 {
            let before = 2 * round - 1;
            let after = before + 1;
            data_checkpoint(&directory, before);
            let mut file = File::open("/tmp/metadata-stable/data-reopen").unwrap();
            data_check_file(&mut errors, "reopened lower", &mut file, &expected);
            close_checked(file);
            data_checkpoint(&directory, after);
            println!("DATA_PHASE {{\"name\":\"cross-fd-reopen\",\"scope\":\"metadata-stable\",\"round\":{round},\"before_token\":{before},\"after_token\":{after},\"bytes\":16384,\"closed_before_next_round\":true}}");
        }
        errors
    });

    data_run(
        &mut cases,
        &mut failed,
        "shared-mmap-copyup-after-close",
        || {
            let path = "/tmp/metadata-stable/data-shared";
            let original = data_pattern(16384, 3);
            let lower = File::open(path).unwrap();
            let mapping = DataMap::new(&lower, 16384, 1, 1);
            assert_eq!(mapping.bytes(0, 4096), original[..4096]);
            close_checked(lower);
            let mut upper = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            let patch = data_pattern(8192, 97);
            upper.write_all_at(&patch, 0).unwrap();
            let mut expected = original.clone();
            expected[..8192].copy_from_slice(&patch);
            let mut errors = Vec::new();
            compare_bytes(
                &mut errors,
                "old shared mapping",
                &mapping.bytes(0, original.len()),
                &original,
            );
            data_check_file(
                &mut errors,
                "upper after shared mapping",
                &mut upper,
                &expected,
            );
            let sync_result = unsafe { msync(mapping.address.cast(), mapping.length, 4) };
            let sync_errno = if sync_result == 0 {
                None
            } else {
                std::io::Error::last_os_error().raw_os_error()
            };
            println!("DATA_OBSERVATION {{\"name\":\"readonly-shared-msync-after-copyup-close\",\"flags\":4,\"returned\":{sync_result},\"errno\":{}}}", sync_errno.map_or_else(|| "null".into(), |value| value.to_string()));
            if sync_result != 0 {
                errors.push(format!("old readonly MAP_SHARED msync(MS_SYNC) expected 0: errno {sync_errno:?}"));
            }
            drop(mapping);
            close_checked(upper);
            errors
        },
    );

    data_run(&mut cases, &mut failed, "private-cow-isolation", || {
        let path = "/tmp/metadata-stable/data-private";
        let original = data_pattern(16384, 3);
        let lower = File::open(path).unwrap();
        let mapping = DataMap::new(&lower, 16384, 3, 2);
        let mut sibling = File::open(path).unwrap();
        bind_without_read(&sibling);
        let denied = unsafe { mmap(std::ptr::null_mut(), 16384, 3, 1, lower.as_raw_fd(), 0) };
        let mut errors = Vec::new();
        if denied as isize != -1 {
            errors.push("read-only FD accepted writable MAP_SHARED".into());
            unsafe {
                munmap(denied, 16384);
            }
        } else if std::io::Error::last_os_error().raw_os_error() != Some(13) {
            errors.push(format!(
                "writable MAP_SHARED denial: {}",
                std::io::Error::last_os_error()
            ));
        }
        close_checked(lower);
        let mut upper = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let patch = data_pattern(8192, 97);
        upper.write_all_at(&patch, 0).unwrap();
        let cow = data_pattern(4096, 173);
        mapping.write(0, &cow);
        assert_eq!(
            unsafe { msync(mapping.address.cast(), mapping.length, 4) },
            0
        );
        let mut private = original.clone();
        private[..4096].copy_from_slice(&cow);
        let mut expected_upper = original.clone();
        expected_upper[..8192].copy_from_slice(&patch);
        compare_bytes(
            &mut errors,
            "private COW",
            &mapping.bytes(0, private.len()),
            &private,
        );
        data_check_file(
            &mut errors,
            "sibling lower after private COW",
            &mut sibling,
            &original,
        );
        data_check_file(
            &mut errors,
            "upper after private COW",
            &mut upper,
            &expected_upper,
        );
        drop(mapping);
        close_checked(sibling);
        close_checked(upper);
        errors
    });

    for extend in [false, true] {
        let name = if extend {
            "mmap-extend-original-eof"
        } else {
            "mmap-shrink-original-eof"
        };
        data_run(&mut cases, &mut failed, name, || {
            let path = if extend {
                "/tmp/metadata-stable/data-mmap-extend"
            } else {
                "/tmp/metadata-stable/data-mmap-shrink"
            };
            let original = data_pattern(8192 + 17, 3);
            let mut lower = File::open(path).unwrap();
            let mapping = DataMap::new(&lower, 16384, 1, 1);
            let mut upper = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            let mut expected = original.clone();
            if extend {
                upper.set_len(16384).unwrap();
                expected.resize(16384, 0);
                let patch = data_pattern(4096, 97);
                upper.write_all_at(&patch, 12288).unwrap();
                expected[12288..].copy_from_slice(&patch);
            } else {
                upper.set_len(4096 + 9).unwrap();
                expected.truncate(4096 + 9);
            }
            let mut errors = Vec::new();
            data_check_file(&mut errors, "old lower size and EOF", &mut lower, &original);
            compare_bytes(
                &mut errors,
                "old mapping valid bytes",
                &mapping.bytes(0, original.len()),
                &original,
            );
            compare_bytes(
                &mut errors,
                "old mapping partial-page zero tail",
                &mapping.bytes(original.len(), 12288 - original.len()),
                &vec![0; 12288 - original.len()],
            );
            mapping.expect_sigbus(12288, &mut errors, "old lower beyond original EOF");
            data_check_file(&mut errors, "upper size and EOF", &mut upper, &expected);
            let current = DataMap::new(&upper, 16384, 1, 1);
            compare_bytes(
                &mut errors,
                "upper mapping valid bytes",
                &current.bytes(0, expected.len()),
                &expected,
            );
            if !extend {
                compare_bytes(
                    &mut errors,
                    "upper partial-page zero tail",
                    &current.bytes(expected.len(), 8192 - expected.len()),
                    &vec![0; 8192 - expected.len()],
                );
                current.expect_sigbus(8192, &mut errors, "upper beyond shortened EOF");
            }
            drop(current);
            drop(mapping);
            close_checked(lower);
            close_checked(upper);
            errors
        });
    }

    data_run(
        &mut cases,
        &mut failed,
        "rename-overwrite-generations",
        || {
            let victim = "/tmp/metadata-stable/data-rename-victim";
            let source = "/tmp/metadata-stable/data-rename-source";
            let (a, b, c, d) = (
                data_pattern(16384, 3),
                data_pattern(16384, 97),
                data_pattern(16384, 173),
                data_pattern(16384, 211),
            );
            let mut old_victim = File::open(victim).unwrap();
            let victim_map = DataMap::new(&old_victim, 16384, 1, 1);
            let mut old_source = File::open(source).unwrap();
            let source_map = DataMap::new(&old_source, 16384, 1, 1);
            fs::rename(source, victim).unwrap();
            let mut upper = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(victim)
                .unwrap();
            upper.write_all_at(&c, 0).unwrap();
            let mut errors = Vec::new();
            data_check_file(&mut errors, "old overwritten victim", &mut old_victim, &a);
            data_check_file(&mut errors, "old rename source", &mut old_source, &b);
            compare_bytes(
                &mut errors,
                "old victim map",
                &victim_map.bytes(0, a.len()),
                &a,
            );
            compare_bytes(
                &mut errors,
                "old source map",
                &source_map.bytes(0, b.len()),
                &b,
            );
            data_check_file(&mut errors, "new victim", &mut upper, &c);
            if !matches!(fs::metadata(source), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            {
                errors.push("rename source still resolves".into());
            }
            fs::write(source, &d).unwrap();
            compare_bytes(
                &mut errors,
                "recreated source",
                &fs::read(source).unwrap(),
                &d,
            );
            compare_bytes(
                &mut errors,
                "old source map after name reuse",
                &source_map.bytes(0, b.len()),
                &b,
            );
            drop(victim_map);
            drop(source_map);
            close_checked(old_victim);
            close_checked(old_source);
            close_checked(upper);
            errors
        },
    );

    data_run(
        &mut cases,
        &mut failed,
        "unlink-recreate-cold-map-after-close",
        || {
            let path = "/tmp/metadata-stable/data-unlink";
            let original = data_pattern(16384, 3);
            let changed = data_pattern(16384, 97);
            let lower = File::open(path).unwrap();
            let mapping = DataMap::new(&lower, 16384, 1, 1);
            close_checked(lower);
            fs::remove_file(path).unwrap();
            fs::write(path, &changed).unwrap();
            let mut errors = Vec::new();
            compare_bytes(
                &mut errors,
                "unlinked cold map",
                &mapping.bytes(0, original.len()),
                &original,
            );
            compare_bytes(
                &mut errors,
                "recreated current name",
                &fs::read(path).unwrap(),
                &changed,
            );
            drop(mapping);
            errors
        },
    );

    data_run(
        &mut cases,
        &mut failed,
        "sendfile-splice-original-source-eof",
        || {
            let path = "/tmp/metadata-stable/data-transfer";
            let original = data_pattern(16384, 3);
            let lower = File::open(path).unwrap();
            bind_without_read(&lower);
            let upper = fs::OpenOptions::new().write(true).open(path).unwrap();
            upper.set_len(4096 + 9).unwrap();
            let raw_output = unsafe { memfd_create(c"data-sendfile".as_ptr(), 1) };
            assert!(
                raw_output >= 0,
                "memfd_create: {}",
                std::io::Error::last_os_error()
            );
            let mut output = unsafe { File::from_raw_fd(raw_output) };
            let mut position = 0i64;
            let mut errors = Vec::new();
            for _ in 0..original.len() / 4096 + 3 {
                let result =
                    unsafe { sendfile(output.as_raw_fd(), lower.as_raw_fd(), &mut position, 4096) };
                if result < 0 {
                    errors.push(format!("sendfile: {}", std::io::Error::last_os_error()));
                    break;
                }
                if result == 0 || position > original.len() as i64 {
                    break;
                }
            }
            data_check_file(&mut errors, "sendfile destination", &mut output, &original);
            if position != original.len() as i64 {
                errors.push(format!("sendfile source EOF position {position}"));
            }
            match data_splice_bytes(&lower, original.len()) {
                Ok(bytes) => compare_bytes(&mut errors, "splice original bytes", &bytes, &original),
                Err(error) => errors.push(error),
            }
            close_checked(output);
            close_checked(lower);
            close_checked(upper);
            errors
        },
    );

    data_run(
        &mut cases,
        &mut failed,
        "physical-hardlink-fallback",
        || {
            let expected = data_pattern(16384, 3);
            let mut errors = Vec::new();
            for path in [
                "/tmp/metadata-stable/data-hardlink",
                "/tmp/metadata-stable/data-hardlink-alias",
            ] {
                let mut file = File::open(path).unwrap();
                data_check_file(
                    &mut errors,
                    "physical nlink2 fallback",
                    &mut file,
                    &expected,
                );
                close_checked(file);
            }
            errors
        },
    );
    for (name, path) in [
        ("mutable", "/tmp/metadata-mutable/data-fallback-mutable"),
        ("upper", "/data-fallback-upper"),
    ] {
        data_run(&mut cases, &mut failed, &format!("{name}-fallback"), || {
            let mut errors = Vec::new();
            let original = data_pattern(16384, 3);
            let changed = data_pattern(16384, 97);
            let mut first = File::open(path).unwrap();
            data_check_file(&mut errors, "fallback before write", &mut first, &original);
            close_checked(first);
            fs::write(path, &changed).unwrap();
            let mut current = File::open(path).unwrap();
            data_check_file(&mut errors, "fallback after write", &mut current, &changed);
            close_checked(current);
            errors
        });
    }

    data_run(
        &mut cases,
        &mut failed,
        "eligible-to-hardlink-copyup",
        || {
            let path = "/tmp/metadata-stable/data-link-transition";
            let alias = "/tmp/metadata-stable/data-link-transition-alias";
            let original = data_pattern(16384, 3);
            let changed = data_pattern(16384, 97);
            let mut lower = File::open(path).unwrap();
            let mapping = DataMap::new(&lower, 16384, 1, 1);
            fs::hard_link(path, alias).unwrap();
            fs::write(alias, &changed).unwrap();
            let mut errors = Vec::new();
            data_check_file(&mut errors, "old lower before alias", &mut lower, &original);
            compare_bytes(
                &mut errors,
                "old lower map before alias",
                &mapping.bytes(0, original.len()),
                &original,
            );
            compare_bytes(
                &mut errors,
                "current hardlink source",
                &fs::read(path).unwrap(),
                &changed,
            );
            compare_bytes(
                &mut errors,
                "current hardlink alias",
                &fs::read(alias).unwrap(),
                &changed,
            );
            let size = statx_record(
                c"/tmp/metadata-stable/data-link-transition",
                STATX_BASIC_STATS,
                AT_STATX_FORCE_SYNC,
            );
            if size.links != 2 {
                errors.push(format!("forced current hardlink nlink {} != 2", size.links));
            }
            drop(mapping);
            close_checked(lower);
            errors
        },
    );
    data_run(
        &mut cases,
        &mut failed,
        "setfl-direct-fixed-lower-eof",
        || {
            let path = "/tmp/metadata-stable/data-direct";
            let original = data_pattern(16384, 3);
            let mut lower = File::open(path).unwrap();
            let mut first = [0];
            assert_eq!(lower.read_at(&mut first, 0).unwrap(), 1);
            assert_eq!(first[0], original[0]);
            let old_flags = unsafe { fcntl(lower.as_raw_fd(), 3) };
            assert!(old_flags >= 0);
            let direct = if cfg!(target_arch = "aarch64") {
                0x10000
            } else {
                0x4000
            };
            assert_eq!(
                unsafe { fcntl(lower.as_raw_fd(), 4, old_flags | direct) },
                0,
                "F_SETFL O_DIRECT: {}",
                std::io::Error::last_os_error()
            );
            let upper = fs::OpenOptions::new().write(true).open(path).unwrap();
            upper.set_len(4096 + 9).unwrap();
            let layout = std::alloc::Layout::from_size_align(4096, 4096).unwrap();
            let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(!pointer.is_null(), "aligned allocation failed");
            let buffer = unsafe { std::slice::from_raw_parts_mut(pointer, 4096) };
            let mut errors = Vec::new();
            match lower.read_at(buffer, 0) {
                Ok(4096) => compare_bytes(
                    &mut errors,
                    "aligned direct pread",
                    buffer,
                    &original[..4096],
                ),
                other => errors.push(format!("aligned direct pread expected 4096: {other:?}")),
            }
            match lower.read_at(buffer, original.len() as u64) {
                Ok(0) => {}
                other => errors.push(format!("aligned direct original EOF expected 0: {other:?}")),
            }
            unsafe { std::alloc::dealloc(pointer, layout) };
            match data_splice_bytes(&lower, original.len()) {
                Ok(bytes) => compare_bytes(
                    &mut errors,
                    "direct splice original lower",
                    &bytes,
                    &original,
                ),
                Err(error) => errors.push(error),
            }
            assert_eq!(unsafe { fcntl(lower.as_raw_fd(), 4, old_flags) }, 0);
            data_check_file(
                &mut errors,
                "cached read after clearing DIRECT",
                &mut lower,
                &original,
            );
            close_checked(lower);
            close_checked(upper);
            errors
        },
    );

    data_run(
        &mut cases,
        &mut failed,
        "map-files-reopen-authorization",
        || {
            let path = "/tmp/metadata-stable/data-map-files";
            let lower = File::open(path).unwrap();
            let mapping = DataMap::new(&lower, 16384, 1, 2);
            let magic = format!(
                "/proc/self/map_files/{:x}-{:x}",
                mapping.address as usize,
                mapping.address as usize + mapping.length
            );
            let upper = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            let current = data_pattern(16384, 97);
            upper.write_all_at(&current, 0).unwrap();
            let target = fs::read_link(&magic).map(|value| value.to_string_lossy().into_owned());
            let directory = File::open("/tmp/metadata-stable").unwrap();
            data_checkpoint(&directory, 7);
            let mut errors = Vec::new();
            match File::open(&magic) {
            Ok(mut file) => {
                // A normal namespace reopen is legal. It must execute normal
                // OPEN and see the current upper, rather than an uninitialized
                // hidden backing file. The phase counters retain that evidence.
                data_check_file(&mut errors, "map_files ordinary reopen", &mut file, &current);
                close_checked(file);
                println!("DATA_OBSERVATION {{\"name\":\"map-files-reopen\",\"status\":\"opened-current-namespace\",\"target\":{}}}", json_string(&format!("{target:?}")));
            }
            Err(error) => println!("DATA_OBSERVATION {{\"name\":\"map-files-reopen\",\"status\":\"denied-or-unavailable\",\"errno\":{},\"target\":{}}}", error.raw_os_error().map_or_else(|| "null".into(), |value| value.to_string()), json_string(&format!("{target:?}"))),
        }
            data_checkpoint(&directory, 8);
            println!("DATA_PHASE {{\"name\":\"map-files-reopen\",\"scope\":\"metadata-stable\",\"round\":0,\"before_token\":7,\"after_token\":8,\"optional_proc_permission\":true}}");
            // On musl, libc::O_ACCMODE includes O_PATH (0o10000000), so
            // Rust OpenOptions masks it out of custom_flags before open().
            // Retain this call as a construction diagnostic; the raw openat
            // below supplies the actual O_PATH + EBADF semantic oracle.
            match fs::OpenOptions::new().read(true).custom_flags(0x20_0000).open(&magic) {
            Ok(file) => {
                let actual_flags = unsafe { fcntl(file.as_raw_fd(), 3) };
                let mut byte = [0]; let result = file.read_at(&mut byte, 0);
                println!("DATA_OBSERVATION {{\"name\":\"map-files-opath\",\"hard_case\":false,\"construction_diagnostic\":true,\"requested_flags\":2097152,\"actual_flags\":{actual_flags},\"actual_opath\":{},\"result\":{}}}", actual_flags >= 0 && actual_flags & 0x20_0000 != 0, json_string(&format!("{result:?}")));
                close_checked(file);
            }
            Err(error) => println!("DATA_OBSERVATION {{\"name\":\"map-files-opath\",\"hard_case\":false,\"construction_diagnostic\":true,\"status\":\"denied-or-unavailable\",\"errno\":{}}}", error.raw_os_error().map_or_else(|| "null".into(), |value| value.to_string())),
        }
            // Same-process controls preserve the observed musl behavior for
            // ordinary paths and proc-fd links without treating it as O_PATH.
            for (kind, control) in [
                ("ordinary-path", path.to_owned()),
                ("proc-fd", format!("/proc/self/fd/{}", lower.as_raw_fd())),
            ] {
                match fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(0x20_0000)
                    .open(&control)
                {
                    Ok(file) => {
                        let actual_flags = unsafe { fcntl(file.as_raw_fd(), 3) };
                        let mut byte = [0];
                        let result = file.read_at(&mut byte, 0);
                        println!("DATA_OBSERVATION {{\"name\":\"map-files-opath-rust-control\",\"target_kind\":{},\"requested_flags\":2097152,\"actual_flags\":{actual_flags},\"actual_opath\":{},\"result\":{}}}", json_string(kind), actual_flags >= 0 && actual_flags & 0x20_0000 != 0, json_string(&format!("{result:?}")));
                        close_checked(file);
                    }
                    Err(error) => println!("DATA_OBSERVATION {{\"name\":\"map-files-opath-rust-control\",\"target_kind\":{},\"status\":\"denied-or-unavailable\",\"errno\":{}}}", json_string(kind), error.raw_os_error().map_or_else(|| "null".into(), |value| value.to_string())),
                }
            }
            let magic_c = std::ffi::CString::new(magic.as_bytes()).unwrap();
            let openat_number = if cfg!(target_arch = "aarch64") { 56 } else { 257 };
            let pread_number = if cfg!(target_arch = "aarch64") { 67 } else { 17 };
            let raw_fd = unsafe {
                syscall(openat_number, -100i32, magic_c.as_ptr(), 0x28_0000i32, 0i32)
            };
            if raw_fd >= 0 {
                let file = unsafe { File::from_raw_fd(raw_fd as i32) };
                let actual_flags = unsafe { fcntl(file.as_raw_fd(), 3) };
                let mut byte = [0u8];
                let returned = unsafe {
                    syscall(pread_number, file.as_raw_fd(), byte.as_mut_ptr(), 1usize, 0i64)
                };
                let read_errno = if returned < 0 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                println!("DATA_OBSERVATION {{\"name\":\"map-files-opath-raw-openat\",\"hard_case\":true,\"openat_syscall\":{openat_number},\"pread_syscall\":{pread_number},\"requested_flags\":2097152,\"syscall_flags\":2621440,\"actual_flags\":{actual_flags},\"actual_opath\":{},\"pread_returned\":{returned},\"pread_errno\":{}}}", actual_flags >= 0 && actual_flags & 0x20_0000 != 0, read_errno.map_or_else(|| "null".into(), |value| value.to_string()));
                if actual_flags < 0 || actual_flags & 0x20_0000 == 0 {
                    errors.push(format!("raw openat O_PATH missing from F_GETFL: {actual_flags}"));
                }
                if returned != -1 || read_errno != Some(9) {
                    errors.push(format!("raw O_PATH magiclink pread expected EBADF: returned {returned}, errno {read_errno:?}"));
                }
                close_checked(file);
            } else {
                let error = std::io::Error::last_os_error();
                println!("DATA_OBSERVATION {{\"name\":\"map-files-opath-raw-openat\",\"hard_case\":true,\"openat_syscall\":{openat_number},\"requested_flags\":2097152,\"syscall_flags\":2621440,\"status\":\"denied-or-unavailable\",\"errno\":{}}}", error.raw_os_error().map_or_else(|| "null".into(), |value| value.to_string()));
            }
            drop(mapping);
            close_checked(lower);
            close_checked(upper);
            errors
        },
    );

    data_copy_range_observation();
    cases += 1;
    failed += usize::from(!review_case("DATA", "unmount-lifetime", || {
        let mut errors = Vec::new();
        for path in [c"/tmp/metadata-stable", c"/tmp/metadata-mutable"] {
            if unsafe { umount(path.as_ptr()) } != 0 {
                errors.push(format!(
                    "umount {path:?}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        errors
    }));
    println!("DATA_PROTOCOL {{\"scope\":\"metadata-stable\",\"tokens\":8,\"intervals\":4,\"open_read_close_rounds\":3,\"reopens_after_first_close\":2,\"copy_file_range_hard_case\":false}}");
    review_summary("review-data", "DATA", cases, failed);
}

fn negative_absent(path: &std::path::Path) {
    let error = fs::symlink_metadata(path).expect_err("name unexpectedly exists");
    assert_eq!(error.raw_os_error(), Some(2), "expected ENOENT: {error}");
}

fn check_review_negative() {
    mount_share("metadata-stable", "/tmp/metadata-stable");
    mount_share("metadata-mutable", "/tmp/metadata-mutable");
    let stable = std::path::Path::new("/tmp/metadata-stable");
    let mutable = std::path::Path::new("/tmp/metadata-mutable");
    let stable_fd = File::open(stable).unwrap();
    let mutable_fd = File::open(mutable).unwrap();
    let mut cases = 0;
    let mut failed = 0;
    let mut run = |name: &str, action: &dyn Fn() -> Vec<String>| {
        cases += 1;
        failed += usize::from(!review_case("NEGATIVE", name, action));
    };

    run("stable-repeated-miss", &|| {
        let path = stable.join("negative-repeat/missing");
        let parent = File::open(path.parent().unwrap()).unwrap();
        data_checkpoint(&stable_fd, 1);
        for _ in 0..32 {
            negative_absent(&path);
        }
        data_checkpoint(&stable_fd, 2);
        close_checked(parent);
        Vec::new()
    });
    run("stable-expiry", &|| {
        let path = stable.join("negative-expiry/missing");
        negative_absent(&path);
        std::thread::sleep(Duration::from_millis(1200));
        data_checkpoint(&stable_fd, 3);
        negative_absent(&path);
        data_checkpoint(&stable_fd, 4);
        Vec::new()
    });
    run("mutable-repeated-miss", &|| {
        let path = mutable.join("negative-repeat/missing");
        let parent = File::open(path.parent().unwrap()).unwrap();
        data_checkpoint(&mutable_fd, 1);
        for _ in 0..32 {
            negative_absent(&path);
        }
        data_checkpoint(&mutable_fd, 2);
        close_checked(parent);
        Vec::new()
    });
    run("create-unlink-recreate", &|| {
        let path = stable.join("negative-create/missing");
        negative_absent(&path);
        fs::write(&path, b"created").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"created");
        assert_eq!(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap_err()
                .raw_os_error(),
            Some(17)
        );
        fs::remove_file(&path).unwrap();
        negative_absent(&path);
        fs::write(&path, b"recreated").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"recreated");
        Vec::new()
    });
    run("mkdir-after-negative", &|| {
        let path = stable.join("negative-mkdir/missing");
        negative_absent(&path);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("child"), b"child").unwrap();
        assert_eq!(fs::read(path.join("child")).unwrap(), b"child");
        fs::remove_dir_all(&path).unwrap();
        negative_absent(&path);
        Vec::new()
    });
    run("symlink-after-negative", &|| {
        let path = stable.join("negative-symlink/missing");
        negative_absent(&path);
        std::os::unix::fs::symlink("existing", &path).unwrap();
        assert_eq!(
            fs::read_link(&path).unwrap(),
            std::path::Path::new("existing")
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        Vec::new()
    });
    run("hardlink-after-negative", &|| {
        let directory = stable.join("negative-link");
        let path = directory.join("missing");
        negative_absent(&path);
        fs::hard_link(directory.join("existing"), &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::metadata(&path).unwrap().nlink(), 2);
        Vec::new()
    });
    run("rename-into-negative", &|| {
        let directory = stable.join("negative-rename");
        let path = directory.join("missing");
        negative_absent(&path);
        fs::rename(directory.join("existing"), &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original");
        negative_absent(&directory.join("existing"));
        Vec::new()
    });
    run("replacement-after-negative", &|| {
        let directory = stable.join("negative-exchange");
        let path = directory.join("missing");
        negative_absent(&path);
        fs::write(&path, b"first").unwrap();
        fs::rename(directory.join("existing"), &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original");
        Vec::new()
    });
    run("failed-rename-then-create", &|| {
        let directory = stable.join("negative-failed");
        let path = directory.join("missing");
        negative_absent(&path);
        assert_eq!(
            fs::rename(directory.join("absent-source"), &path)
                .unwrap_err()
                .raw_os_error(),
            Some(2)
        );
        negative_absent(&path);
        fs::write(&path, b"after-failure").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"after-failure");
        Vec::new()
    });
    run("parent-rename-with-negative-child", &|| {
        let directory = stable.join("negative-parent");
        negative_absent(&directory.join("missing"));
        let moved = stable.join("negative-parent-moved");
        negative_absent(&moved);
        fs::rename(&directory, &moved).unwrap();
        negative_absent(&moved.join("missing"));
        fs::write(moved.join("missing"), b"moved-child").unwrap();
        assert_eq!(fs::read(moved.join("missing")).unwrap(), b"moved-child");
        negative_absent(&directory);
        Vec::new()
    });
    run("permission-check-on-cached-negative", &|| {
        unsafe extern "C" {
            fn setgid(gid: u32) -> i32;
            fn setuid(uid: u32) -> i32;
        }
        let directory = stable.join("negative-permissions");
        let path = directory.join("missing");
        negative_absent(&path);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0)).unwrap();
        let child = unsafe { fork() };
        assert!(child >= 0);
        if child == 0 {
            let code = if unsafe { setgid(65534) } != 0 || unsafe { setuid(65534) } != 0 {
                2
            } else if fs::symlink_metadata(&path)
                .err()
                .and_then(|e| e.raw_os_error())
                == Some(13)
            {
                0
            } else {
                3
            };
            unsafe { _exit(code) }
        }
        let status = data_wait(child);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(status, 0, "permission child status={status}");
        negative_absent(&path);
        Vec::new()
    });
    run("concurrent-negative-create-unlink", &|| {
        let directory = stable.join("negative-concurrent");
        let path = directory.join("missing");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let reader_barrier = barrier.clone();
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut errors = Vec::new();
            for iteration in 0..100 {
                if fs::symlink_metadata(&reader_path)
                    .err()
                    .and_then(|e| e.raw_os_error())
                    != Some(2)
                {
                    errors.push(format!("iteration {iteration}: expected initial ENOENT"));
                }
                reader_barrier.wait();
                reader_barrier.wait();
                if !fs::read(&reader_path).is_ok_and(|bytes| bytes == b"visible") {
                    errors.push(format!("iteration {iteration}: published file not visible"));
                }
                reader_barrier.wait();
                reader_barrier.wait();
            }
            errors
        });
        for _ in 0..100 {
            barrier.wait();
            let pending = directory.join("pending");
            fs::write(&pending, b"visible").unwrap();
            fs::rename(&pending, &path).unwrap();
            barrier.wait();
            barrier.wait();
            fs::remove_file(&path).unwrap();
            barrier.wait();
        }
        reader.join().unwrap()
    });
    run("mutable-external-create", &|| {
        let path = mutable.join("negative-external/arrived");
        negative_absent(&path);
        fs::write("/metadata-ready", b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(Instant::now() < deadline, "external creation not visible");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fs::read(&path).unwrap(), b"external-visible");
        Vec::new()
    });
    run("excluded-existing-and-child", &|| {
        let hidden = stable.join("negative-hidden");
        data_checkpoint(&stable_fd, 5);
        for _ in 0..32 {
            negative_absent(&hidden.join("existing"));
        }
        data_checkpoint(&stable_fd, 6);
        let error = File::open(hidden.join("existing")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(2));
        let error = fs::write(hidden.join("created"), b"forbidden").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(2));
        Vec::new()
    });
    run("excluded-ancestor-fallback", &|| {
        let parent = stable.join("negative-excluded-ancestor");
        data_checkpoint(&stable_fd, 7);
        for _ in 0..32 {
            negative_absent(&parent);
        }
        data_checkpoint(&stable_fd, 8);
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("visible"), b"visible").unwrap();
        assert_eq!(fs::read(parent.join("visible")).unwrap(), b"visible");
        let error = fs::write(parent.join("secret"), b"forbidden").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(2));
        Vec::new()
    });
    close_checked(stable_fd);
    close_checked(mutable_fd);
    for path in [c"/tmp/metadata-stable", c"/tmp/metadata-mutable"] {
        assert_eq!(unsafe { umount(path.as_ptr()) }, 0);
    }
    review_summary("review-negative", "NEGATIVE", cases, failed);
}

fn main() {
    let mode = std::env::args().nth(1).unwrap();
    if mode == "review-negative" {
        check_review_negative();
        return;
    }
    if mode == "review-data" {
        check_review_data();
        return;
    }
    if mode == "review-pages" {
        check_review_pages();
        return;
    }
    if mode == "review-flush" {
        check_review_flush();
        return;
    }
    if mode == "review-attrs" {
        check_review_attrs();
        return;
    }
    if mode == "atime" {
        check_atime();
        return;
    }
    if mode == "review-poll" {
        mount_share("metadata-mutable", "/tmp/metadata-mutable");
        let held = File::open("/tmp/metadata-mutable/poll-target").unwrap();
        fs::write("/metadata-ready", b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::path::Path::new("/tmp/metadata-mutable/poll-removed").exists() {
            assert!(Instant::now() < deadline, "host removal did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut descriptor = PollFd {
            fd: held.as_raw_fd(),
            events: 1,
            revents: 0,
        };
        assert_eq!(unsafe { poll(&mut descriptor, 1, 0) }, 1);
        assert_eq!(
            descriptor.revents, 8,
            "OPEN failure must produce only POLLERR"
        );
        println!("GUEST_METADATA_PASS review-poll");
        return;
    }
    if mode == "bench" {
        mount_share("metadata-stable", "/tmp/metadata-stable");
        for round in 1..=3 {
            let started = Instant::now();
            for index in 0..256 {
                assert_eq!(
                    fs::metadata(format!("/tmp/metadata-stable/wide-lookup/f{index:04}"))
                        .unwrap()
                        .len(),
                    8
                );
            }
            println!(
                "SDK_PHASE {{\"round\":{round},\"phase\":\"stat256\",\"ms\":{}}}",
                started.elapsed().as_secs_f64() * 1000.0
            );
            let started = Instant::now();
            assert_eq!(fs::read_dir("/tmp/metadata-stable").unwrap().count(), 302);
            for index in 0..300 {
                assert_eq!(
                    fs::metadata(format!("/tmp/metadata-stable/dir-{index:04}/file"))
                        .unwrap()
                        .len(),
                    1
                );
            }
            println!(
                "SDK_PHASE {{\"round\":{round},\"phase\":\"scan300\",\"ms\":{}}}",
                started.elapsed().as_secs_f64() * 1000.0
            );
            let started = Instant::now();
            for _ in 0..10000 {
                assert_eq!(
                    File::open("/metadata-only")
                        .unwrap()
                        .metadata()
                        .unwrap()
                        .len(),
                    8
                );
            }
            println!(
                "SDK_PHASE {{\"round\":{round},\"phase\":\"open10000\",\"ms\":{}}}",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        assert_eq!(unsafe { umount(c"/tmp/metadata-stable".as_ptr()) }, 0);
        println!("GUEST_METADATA_PASS bench");
        return;
    }
    if mode.starts_with("review") {
        check_lifetime(&mode);
        println!("GUEST_METADATA_PASS {mode}");
        return;
    }
    assert_eq!(
        File::open("/metadata-only")
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        8
    );
    if mode != "baseline" {
        for _ in 0..1000 {
            let mut file = File::open("/metadata-only").unwrap();
            assert_eq!(file.metadata().unwrap().len(), 8);
            assert_eq!(file.seek(SeekFrom::Current(0)).unwrap(), 0);
            assert_eq!(file.seek(SeekFrom::End(0)).unwrap(), 8);
            assert_eq!(file.seek(SeekFrom::Start(0)).unwrap(), 0);
        }
    }
    if mode == "full" {
        // Exercise mutations against a scanned image, not just native dentries.
        assert!(fs::read_dir("/").unwrap().count() > 0);
        let mut file = File::open("/metadata-only").unwrap();
        fs::remove_file("/metadata-only").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"metadata", "unlink lost the original open object");
        drop(file);
        fs::write("/replacement-source", b"new").unwrap();
        fs::write("/replacement-target", b"original").unwrap();
        let mut original = File::open("/replacement-target").unwrap();
        fs::rename("/replacement-source", "/replacement-target").unwrap();
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
        assert_eq!(fs::read("/replacement-target").unwrap(), b"new");
        drop(original);
        let mut lower = File::open("/copyup-source").unwrap();
        assert_eq!(lower.metadata().unwrap().len(), 8);
        fs::write("/copyup-source", b"modified").unwrap();
        let mut bytes = Vec::new();
        lower.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original", "copy-up changed the open lower object");
        assert_eq!(fs::read("/copyup-source").unwrap(), b"modified");
        drop(lower);
        for i in 0..32 {
            let name = format!("/concurrent-{i}");
            fs::write(&name, b"retained").unwrap();
            let mut reader = File::open(&name).unwrap();
            let duplicate = reader.try_clone().unwrap();
            let remover = std::thread::spawn(move || {
                fs::remove_file(name).unwrap();
                drop(duplicate);
            });
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"retained");
            remover.join().unwrap();
        }
        mount_share("metadata-mutable", "/tmp/metadata-mutable");
        assert!(
            fs::read_dir("/tmp/metadata-mutable")
                .unwrap()
                .next()
                .is_none()
        );
        let created = "/tmp/metadata-mutable/created-after-import";
        assert!(!std::path::Path::new(created).exists());
        fs::write("/metadata-ready", b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match fs::read(created) {
                Ok(bytes) => {
                    assert_eq!(bytes, b"visible");
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    assert!(
                        Instant::now() < deadline,
                        "mutable directory hid an external create"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("external create: {e}"),
            }
        }
        assert_eq!(unsafe { umount(c"/tmp/metadata-mutable".as_ptr()) }, 0);
        mount_share("metadata-stable", "/tmp/metadata-stable");
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let scanner_start = start.clone();
        let scanner = std::thread::spawn(move || {
            scanner_start.wait();
            assert_eq!(
                fs::read_dir("/tmp/metadata-stable")
                    .unwrap()
                    .map(Result::unwrap)
                    .count(),
                302
            );
        });
        start.wait();
        for _ in 0..3 {
            for i in 0..300 {
                assert_eq!(
                    fs::read(format!("/tmp/metadata-stable/dir-{i:04}/file")).unwrap(),
                    b"x"
                );
            }
        }
        scanner.join().unwrap();
        for i in 0..300 {
            let directory = format!("/tmp/metadata-stable/dir-{i:04}");
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            let file = File::open(format!("{directory}/file")).unwrap();
            assert_eq!(file.metadata().unwrap().len(), 1);
        }
        let wide = "/tmp/metadata-stable/wide-lookup";
        for i in 128..192 {
            assert_eq!(fs::metadata(format!("{wide}/f{i:04}")).unwrap().len(), 8);
        }
        assert_eq!(
            fs::read(format!("{wide}/f0255")).unwrap(),
            255u64.to_le_bytes()
        );
        assert!(!std::path::Path::new(&format!("{wide}/f0256")).exists());
        fs::write(format!("{wide}/created"), b"created!").unwrap();
        let mut names: Vec<_> = fs::read_dir(wide)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        let mut expected: Vec<_> = (0..256)
            .map(|i| std::ffi::OsString::from(format!("f{i:04}")))
            .collect();
        expected.push("created".into());
        expected.sort();
        assert_eq!(
            names, expected,
            "partial-image mutation lost directory entries"
        );
        let dense = "/tmp/metadata-stable/dense-lookup";
        for i in 0u64..256 {
            assert_eq!(
                fs::read(format!("{dense}/f{i:04}")).unwrap(),
                i.to_le_bytes()
            );
        }
        let mut held = File::open(format!("{dense}/f0001")).unwrap();
        fs::remove_file(format!("{dense}/f0001")).unwrap();
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, 1u64.to_le_bytes());
        drop(held);
        assert!(!std::path::Path::new(&format!("{dense}/f0001")).exists());
        // Cached directory images must not retain child dentries across unmount.
        assert_eq!(unsafe { umount(c"/tmp/metadata-stable".as_ptr()) }, 0);
    }
    println!("GUEST_METADATA_PASS {mode}");
}

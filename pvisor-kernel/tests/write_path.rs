//! Static workload for the Rust native write path and guest-local output reference.

use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, PermissionsExt},
    },
    path::Path,
    time::Instant,
};

const BLOCKS: usize = 4096;
const BLOCK_SIZE: usize = 1024;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

unsafe extern "C" {
    fn setgroups(count: usize, groups: *const u32) -> i32;
    fn setgid(gid: u32) -> i32;
    fn setuid(uid: u32) -> i32;
    fn capget(header: *mut CapHeader, data: *mut u32) -> i32;
    fn syncfs(fd: i32) -> i32;
}

fn enter_unprivileged() {
    assert_eq!(unsafe { setgroups(0, std::ptr::null()) }, 0);
    assert_eq!(unsafe { setgid(1000) }, 0);
    assert_eq!(unsafe { setuid(1000) }, 0);
    let mut header = CapHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut caps = [u32::MAX; 6];
    assert_eq!(unsafe { capget(&mut header, caps.as_mut_ptr()) }, 0);
    assert_eq!((caps[0], caps[3]), (0, 0));
}

fn block(index: usize) -> [u8; BLOCK_SIZE] {
    std::array::from_fn(|i| (i * 17 + index * 31 + index / 23) as u8)
}

fn verify(path: &Path) {
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len(), BLOCK_SIZE * BLOCKS);
    for (index, chunk) in bytes.chunks_exact(BLOCK_SIZE).enumerate() {
        assert!(chunk == block(index), "incorrect block {index}");
    }
}

fn assert_setid_invalidation(directory: &Path) {
    let file = File::create_new(directory.join("privileges")).unwrap();
    file.write_all_at(b"a", 0).unwrap();
    file.write_all_at(b"b", 0).unwrap();
    for (before, after) in [(0o6755, 0o0755), (0o6644, 0o2644)] {
        file.set_permissions(Permissions::from_mode(before))
            .unwrap();
        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o7777,
            before
        );
        file.write_all_at(b"c", 0).unwrap();
        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o7777,
            after
        );
    }
}

fn write_output(local: bool) {
    let shared = Path::new("/output");
    let local_root = Path::new("/dev/shm/native-write-path");
    if local {
        fs::create_dir(local_root).unwrap();
        fs::set_permissions(local_root, Permissions::from_mode(0o777)).unwrap();
    }
    enter_unprivileged();
    let work = if local { local_root } else { shared };
    assert_setid_invalidation(work);
    let input: Vec<_> = (0..BLOCKS).map(block).collect();
    let start = Instant::now();
    let pending = work.join(".pending");
    let mut file = File::create_new(&pending).unwrap();
    for bytes in &input {
        file.write_all(bytes).unwrap();
    }
    let written = start.elapsed();
    file.sync_all().unwrap();
    drop(file);
    let ready = work.join("ready");
    fs::rename(&pending, &ready).unwrap();
    File::open(work).unwrap().sync_all().unwrap();
    let work_done = start.elapsed();

    if local {
        let mut source = File::open(&ready).unwrap();
        let pending = shared.join(".pending");
        let mut target = File::create_new(&pending).unwrap();
        let mut buf = vec![0; 256 * 1024];
        // Bound guest scratch and make export batching independent of the stdlib's
        // platform-specific copy_file_range/sendfile selection.
        loop {
            let n = io::Read::read(&mut source, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            target.write_all(&buf[..n]).unwrap();
        }
        target.sync_all().unwrap();
        drop(target);
        fs::rename(&pending, shared.join("ready")).unwrap();
        File::open(shared).unwrap().sync_all().unwrap();
    }
    let published = start.elapsed();
    verify(&shared.join("ready"));
    println!(
        "NATIVE_WRITE_OK write_ns={} work_ns={} publish_ns={}",
        written.as_nanos(),
        work_done.as_nanos(),
        published.as_nanos()
    );
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [verify_arg, path] if verify_arg == "--verify" => verify(Path::new(path)),
        [mode] if matches!(mode.as_str(), "shared" | "local") => write_output(mode == "local"),
        [mode] if mode == "syncfs" => {
            let dir = File::open("/output").unwrap();
            assert_eq!(unsafe { syncfs(dir.as_raw_fd()) }, 0);
            println!("SYNCFS_OK");
        }
        [mode] if mode == "async-error" => {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open("/output/failure")
                .unwrap();
            assert_eq!((&file).write(&vec![0x5a; 256 * 1024]).unwrap(), 256 * 1024);
            println!("ASYNC_WRITE_ACCEPTED");
        }
        _ => panic!("expected shared, local, syncfs, async-error or --verify PATH"),
    }
}

#![cfg(target_os = "linux")]
use pvisor_vm::api::{RamFileMapping, RamFileMount, RamFileStore};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    os::unix::fs::FileExt,
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};
struct Store(File);
impl RamFileStore for Store {
    fn logical_bytes(&self) -> u64 {
        self.0.metadata().unwrap().len()
    }
    fn set_len(&mut self, n: u64) -> io::Result<()> {
        self.0.set_len(n)
    }
    fn read_at(&self, o: u64, b: &mut [u8]) -> io::Result<usize> {
        self.0.read_at(b, o)
    }
    fn write_at(&mut self, o: u64, b: &[u8]) -> io::Result<()> {
        self.0.write_all_at(b, o)
    }
    fn flush_writes(&self) -> io::Result<()> {
        self.0.sync_all()
    }
}
/// Real FUSE and namespace test: the child retains only a copied mount, not a RAM fd.
#[test]
#[ignore = "requires /dev/fuse and permitted user/mount namespaces"]
fn inherited_namespace_does_not_retain_ram_serving_thread() {
    let root = tempfile::tempdir().unwrap();
    let backing = tempfile::tempfile().unwrap();
    backing.set_len(65536).unwrap();
    let backing_check = backing.try_clone().unwrap();
    let (mount, file) =
        RamFileMount::mount(Arc::new(Mutex::new(Store(backing))), root.path()).unwrap();
    // Exercise mmap dirty writeback before closing the last legitimate consumer.
    unsafe {
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            65536,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            std::os::fd::AsRawFd::as_raw_fd(&file),
            0,
        );
        assert_ne!(ptr, libc::MAP_FAILED);
        std::ptr::write_bytes(ptr, 0x5a, 65536);
        assert_eq!(libc::msync(ptr, 65536, libc::MS_SYNC), 0);
        assert_eq!(libc::munmap(ptr, 65536), 0);
    }
    file.sync_all().unwrap();
    let mut bytes = vec![0; 65536];
    backing_check.read_exact_at(&mut bytes, 0).unwrap();
    assert!(bytes.iter().all(|b| *b == 0x5a));
    let mut child=Command::new("unshare").args(["-Urnm","--","sh","-c","mount --make-rprivate / && grep -q pvisor-pvzram /proc/self/mountinfo && echo ready; read finish"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    if ready.trim() != "ready" {
        let _ = child.kill();
        let _ = child.wait();
        panic!("namespace fixture did not become ready");
    }
    drop(file);
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let worker = std::thread::spawn(move || {
        drop(mount);
        let _ = tx.send(());
    });
    let result = rx.recv_timeout(Duration::from_secs(5));
    let holder_alive = child.try_wait().unwrap().is_none();
    // Always release our own namespace, including on the unfixed implementation.
    let _ = child.stdin.take().unwrap().write_all(b"done\n");
    let status = child.wait().unwrap();
    worker.join().unwrap();
    assert!(status.success());
    assert!(holder_alive);
    assert!(
        result.is_ok(),
        "RAM owner waited for an unrelated namespace (elapsed {:?})",
        started.elapsed()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

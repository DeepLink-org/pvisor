//! Standalone Linux/musl guest payload; compile for the VM's architecture.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn umount(target: *const std::ffi::c_char) -> i32;
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

fn main() {
    let mode = std::env::args().nth(1).unwrap();
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
        assert!(fs::read_dir("/tmp/metadata-mutable")
            .unwrap()
            .next()
            .is_none());
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
        for i in 0..300 {
            let file = File::open(format!("/tmp/metadata-stable/dir-{i:04}/file")).unwrap();
            assert_eq!(file.metadata().unwrap().len(), 1);
        }
        // Cached directory images must not retain child dentries across unmount.
        assert_eq!(unsafe { umount(c"/tmp/metadata-stable".as_ptr()) }, 0);
    }
    println!("GUEST_METADATA_PASS {mode}");
}

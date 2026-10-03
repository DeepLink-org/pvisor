//! Exercise the real virtio-fs worker through guest descriptor rings.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
use devices::{
    legacy::{GicV3, IrqChipDevice, VcpuList},
    virtio::{
        DeviceQueue, DeviceSnapshot, InterruptTransport, Queue, VirtioDevice,
        fs::{Fs, fuse, passthrough::PermissionSemantics},
    },
};
use std::{
    sync::{Arc, Mutex, atomic::AtomicI32},
    time::{Duration, Instant},
};
use utils::eventfd::{EFD_NONBLOCK, EventFd};
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemoryMmap};

struct GuestFs {
    fs: Fs,
    mem: GuestMemoryMmap,
    events: Vec<Arc<EventFd>>,
    next: u16,
}
impl GuestFs {
    fn new(
        root: &std::path::Path,
        restore: Option<(GuestMemoryMmap, DeviceSnapshot, u16)>,
    ) -> Self {
        let fs = Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            Some(root.to_str().unwrap().into()),
            Arc::new(AtomicI32::new(0)),
            false,
            vec![],
            None,
        )
        .unwrap();
        Self::from_device(fs, restore)
    }
    fn from_device(mut fs: Fs, restore: Option<(GuestMemoryMmap, DeviceSnapshot, u16)>) -> Self {
        let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
            Arc::new(VcpuList::new(1)),
        )))));
        let interrupt = InterruptTransport::new(chip, "fs-check".into()).unwrap();
        let events: Vec<_> = (0..2)
            .map(|_| Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()))
            .collect();
        let restoring = restore.is_some();
        let (mem, queues, next) = if let Some((mem, state, next)) = restore {
            fs.restore_state(&state.state).unwrap();
            let queues = state
                .queues
                .unwrap()
                .iter()
                .map(|q| q.restore(1024, &mem).unwrap())
                .collect::<Vec<_>>();
            (mem, queues, next)
        } else {
            let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
            let queues = [(0x1000, 0x5000, 0x6000), (0x8000, 0xc000, 0xd000)]
                .into_iter()
                .map(|(desc, avail, used)| {
                    let mut q = Queue::new(1024);
                    q.size = 1024;
                    q.ready = true;
                    q.desc_table = GuestAddress(desc);
                    q.avail_ring = GuestAddress(avail);
                    q.used_ring = GuestAddress(used);
                    q
                })
                .collect::<Vec<_>>();
            (mem, queues, 0)
        };
        fs.activate(
            mem.clone(),
            interrupt,
            queues
                .into_iter()
                .zip(events.iter().cloned())
                .map(|(q, e)| DeviceQueue::new(q, e))
                .collect(),
        )
        .unwrap();
        if restoring {
            fs.thaw().unwrap();
        }
        Self {
            fs,
            mem,
            events,
            next,
        }
    }
    fn request(&mut self, opcode: fuse::Opcode, inode: u64, payload: &[u8]) -> Vec<u8> {
        let input = fuse::InHeader {
            len: (std::mem::size_of::<fuse::InHeader>() + payload.len()) as u32,
            opcode: opcode as u32,
            unique: u64::from(self.next) + 1,
            nodeid: inode,
            uid: 0,
            gid: 0,
            pid: 1,
            ..Default::default()
        };
        self.mem
            .write_slice(input.as_slice(), GuestAddress(0x20000))
            .unwrap();
        self.mem
            .write_slice(
                payload,
                GuestAddress(0x20000 + input.as_slice().len() as u64),
            )
            .unwrap();
        // A readable request followed by a writable response.
        for (index, address, len, flags, next) in [
            (0_u64, 0x20000_u64, input.len, 1_u16, 1_u16),
            (1, 0x28000, 0x8000, 2, 0),
        ] {
            let base = 0x8000 + index * 16;
            self.mem.write_obj(address, GuestAddress(base)).unwrap();
            self.mem.write_obj(len, GuestAddress(base + 8)).unwrap();
            self.mem.write_obj(flags, GuestAddress(base + 12)).unwrap();
            self.mem.write_obj(next, GuestAddress(base + 14)).unwrap();
        }
        self.mem
            .write_obj(
                0_u16,
                GuestAddress(0xc004 + u64::from(self.next % 1024) * 2),
            )
            .unwrap();
        self.next = self.next.wrapping_add(1);
        self.mem.write_obj(self.next, GuestAddress(0xc002)).unwrap();
        self.events[1].write(1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.mem.read_obj::<u16>(GuestAddress(0xd002)).unwrap() != self.next {
            assert!(Instant::now() < deadline, "FUSE worker timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        let response: fuse::OutHeader = self.mem.read_obj(GuestAddress(0x28000)).unwrap();
        assert_eq!(response.error, 0, "FUSE {opcode:?} failed");
        let mut result = vec![0; response.len as usize - std::mem::size_of::<fuse::OutHeader>()];
        self.mem
            .read_slice(
                &mut result,
                GuestAddress(0x28000 + std::mem::size_of::<fuse::OutHeader>() as u64),
            )
            .unwrap();
        result
    }
    fn freeze(&mut self) -> DeviceSnapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.fs.freeze().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        serde_json::from_slice(&serde_json::to_vec(&self.fs.capture_state().unwrap()).unwrap())
            .unwrap()
    }
    fn read(&mut self, inode: u64, handle: u64) -> Vec<u8> {
        self.request(
            fuse::Opcode::Read,
            inode,
            fuse::ReadIn {
                fh: handle,
                offset: 3,
                size: 4,
                ..Default::default()
            }
            .as_slice(),
        )
    }
}
impl Drop for GuestFs {
    fn drop(&mut self) {
        self.fs.reset();
    }
}

#[test]
fn fs_rebinds_verified_copy_after_original_tree_is_removed() {
    use devices::virtio::DeviceSnapshotState;
    use std::os::unix::fs::{MetadataExt, symlink};

    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("original");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("data"), b"0123456789").unwrap();
    symlink("data", root.join("link")).unwrap();
    let root = root.canonicalize().unwrap();
    let mut source = GuestFs::new(&root, None);
    source.request(
        fuse::Opcode::Init,
        0,
        fuse::InitInCompat {
            major: 7,
            minor: 31,
            ..Default::default()
        }
        .as_slice(),
    );
    let entry = source.request(fuse::Opcode::Lookup, 1, b"data\0");
    let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    source.request(fuse::Opcode::Lookup, 1, b"link\0");
    let opened = source.request(
        fuse::Opcode::Open,
        inode,
        fuse::OpenIn::default().as_slice(),
    );
    let handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let mut state = source.freeze();
    let copied = workspace.path().join("copied");
    assert!(
        std::process::Command::new("/bin/cp")
            .args(["-pR"])
            .arg(&root)
            .arg(&copied)
            .status()
            .unwrap()
            .success()
    );
    assert_ne!(
        std::fs::metadata(root.join("data")).unwrap().ino(),
        std::fs::metadata(copied.join("data")).unwrap().ino()
    );
    let damaged = workspace.path().join("damaged");
    assert!(
        std::process::Command::new("/bin/cp")
            .arg("-pR")
            .arg(&root)
            .arg(&damaged)
            .status()
            .unwrap()
            .success()
    );
    let mem = source.mem.clone();
    let next = source.next;
    drop(source);
    std::fs::remove_dir_all(&root).unwrap();

    let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
        panic!()
    };
    let pristine = serde_json::to_vec(server).unwrap();
    assert!(server.rebind_owned_copy(&copied, &copied).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    std::fs::remove_file(damaged.join("link")).unwrap();
    symlink("evil", damaged.join("link")).unwrap();
    assert!(server.rebind_owned_copy(&root, &damaged).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    std::fs::remove_file(damaged.join("link")).unwrap();
    symlink("data", damaged.join("link")).unwrap();
    std::fs::write(damaged.join("data"), b"XXXXXXXXXX").unwrap();
    assert!(server.rebind_owned_copy(&root, &damaged).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    server.rebind_owned_copy(&root, &copied).unwrap();
    let mut destination = GuestFs::new(&copied, Some((mem, state, next)));
    assert_eq!(destination.read(inode, handle), b"3456");
}

#[test]
fn fs_restores_inode_and_open_handle_without_a_new_fuse_init() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("data"), b"0123456789").unwrap();
    let mut source = GuestFs::new(root.path(), None);
    source.request(
        fuse::Opcode::Init,
        0,
        fuse::InitInCompat {
            major: 7,
            minor: 31,
            ..Default::default()
        }
        .as_slice(),
    );
    let entry = source.request(fuse::Opcode::Lookup, 1, b"data\0");
    let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    let opened = source.request(
        fuse::Opcode::Open,
        inode,
        fuse::OpenIn::default().as_slice(),
    );
    let handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    assert_eq!(source.read(inode, handle), b"3456");
    assert!(source.fs.capture_state().is_err());
    let state = source.freeze();
    let original = serde_json::to_value(&state).unwrap();
    for corruption in ["path", "allocator", "flags", "tag"] {
        let mut value = original.clone();
        match corruption {
            "path" => {
                value["state"]["state"]["server"]["fs"]["state"]["inner"]["state"]["inodes"][1]["path"] =
                    serde_json::json!(b"../outside".to_vec())
            }
            "allocator" => value["state"]["state"]["server"]["next_inode"] = 1.into(),
            "flags" => {
                value["state"]["state"]["server"]["fs"]["state"]["inner"]["state"]["handles"][0]["flags"] =
                    0x200.into()
            } // native O_TRUNC
            "tag" => value["state"]["state"]["tag"] = serde_json::json!([1]),
            _ => unreachable!(),
        }
        let invalid: DeviceSnapshot = serde_json::from_value(value).unwrap();
        let mut fresh = Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            Some(root.path().to_str().unwrap().into()),
            Arc::new(AtomicI32::new(0)),
            false,
            vec![],
            None,
        )
        .unwrap();
        if corruption == "tag" {
            assert!(fresh.restore_state(&invalid.state).is_err());
            continue;
        }
        fresh.restore_state(&invalid.state).unwrap();
        let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
            Arc::new(VcpuList::new(1)),
        )))));
        let interrupt = InterruptTransport::new(chip, "reject".into()).unwrap();
        let queues = invalid
            .queues
            .unwrap()
            .iter()
            .map(|q| {
                DeviceQueue::new(
                    q.restore(1024, &source.mem).unwrap(),
                    Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
                )
            })
            .collect();
        assert!(
            fresh
                .activate(source.mem.clone(), interrupt, queues)
                .is_err(),
            "{corruption}"
        );
        assert_eq!(
            std::fs::read(root.path().join("data")).unwrap(),
            b"0123456789"
        );
    }
    let mem = source.mem.clone();
    let next = source.next;
    drop(source);
    let mut destination = GuestFs::new(root.path(), Some((mem, state, next)));
    assert_eq!(destination.read(inode, handle), b"3456");
    let state = destination.freeze();
    std::fs::write(root.path().join("data"), b"modified!!").unwrap();
    let mut fresh = Fs::new(
        "rootfs".into(),
        PermissionSemantics::LinuxComplete,
        Some(root.path().to_str().unwrap().into()),
        Arc::new(AtomicI32::new(0)),
        false,
        vec![],
        None,
    )
    .unwrap();
    fresh.restore_state(&state.state).unwrap();
    // Validation occurs while building the parked worker, before it can serve
    // any old guest request. Changed backing content must reject activation.
    let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
        Arc::new(VcpuList::new(1)),
    )))));
    let interrupt = InterruptTransport::new(chip, "reject".into()).unwrap();
    let queues = state
        .queues
        .unwrap()
        .iter()
        .map(|q| {
            DeviceQueue::new(
                q.restore(1024, &destination.mem).unwrap(),
                Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
            )
        })
        .collect();
    assert!(
        fresh
            .activate(destination.mem.clone(), interrupt, queues)
            .is_err()
    );
}

#[test]
fn overlay_preserves_directory_cookies_and_consumed_virtual_names() {
    use devices::virtio::fs::{
        OverlayConfig,
        virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent},
    };
    let lower = tempfile::tempdir().unwrap();
    let upper = tempfile::tempdir().unwrap();
    std::fs::write(lower.path().join("data"), b"0123456789").unwrap();
    let make = || {
        Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            None,
            Arc::new(AtomicI32::new(0)),
            false,
            vec![VirtualDirEntry {
                name: std::ffi::CString::new("once").unwrap(),
                entry: VirtualEntry {
                    mode: 0o400,
                    one_shot: true,
                    content: VirtualEntryContent::File { data: b"static" },
                },
            }],
            Some(OverlayConfig {
                lower_dirs: vec![lower.path().to_str().unwrap().into()],
                upper_dir: upper.path().to_str().unwrap().into(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            }),
        )
        .unwrap()
    };
    let mut source = GuestFs::from_device(make(), None);
    source.request(
        fuse::Opcode::Init,
        0,
        fuse::InitInCompat {
            major: 7,
            minor: 31,
            ..Default::default()
        }
        .as_slice(),
    );
    let entry = source.request(fuse::Opcode::Lookup, 1, b"once\0");
    let virtual_inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    let opened = source.request(fuse::Opcode::Opendir, 1, fuse::OpenIn::default().as_slice());
    let directory = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let read = fuse::ReadIn {
        fh: directory,
        offset: 0,
        size: 4096,
        ..Default::default()
    };
    let listing = source.request(fuse::Opcode::Readdir, 1, read.as_slice());
    let first: fuse::Dirent =
        *fuse::Dirent::from_slice(&listing[..std::mem::size_of::<fuse::Dirent>()]).unwrap();
    let remainder = source.request(
        fuse::Opcode::Readdir,
        1,
        fuse::ReadIn {
            offset: first.off,
            ..read
        }
        .as_slice(),
    );
    let state = source.freeze();
    let mem = source.mem.clone();
    let next = source.next;
    drop(source);
    let mut restored = GuestFs::from_device(make(), Some((mem, state, next)));
    assert_eq!(
        restored.request(
            fuse::Opcode::Readdir,
            1,
            fuse::ReadIn {
                offset: first.off,
                ..read
            }
            .as_slice()
        ),
        remainder
    );
    let state = restored.freeze();
    // Consumed one-shot name stays absent, but an already looked-up virtual
    // inode remains available until release. A fresh init would resurrect it.
    let devices::virtio::DeviceSnapshotState::Fs { server, .. } = state.state else {
        panic!()
    };
    let value = serde_json::to_value(server).unwrap();
    let names = value["fs"]["state"]["names"].as_array().unwrap();
    assert!(names.is_empty());
    assert!(
        value["fs"]["state"]["inodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i[0] == virtual_inode)
    );
}

#[test]
fn fs_restore_in_new_process_keeps_the_old_guest_handle() {
    const ROLE: &str = "PVISOR_FS_SNAPSHOT_CHECK_ROLE";
    const DIRECTORY: &str = "PVISOR_FS_SNAPSHOT_CHECK_DIRECTORY";
    if let Ok(role) = std::env::var(ROLE) {
        let directory = std::path::PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
        let root = directory.join("root");
        if role == "save" {
            let mut source = GuestFs::new(&root, None);
            source.request(
                fuse::Opcode::Init,
                0,
                fuse::InitInCompat {
                    major: 7,
                    minor: 31,
                    ..Default::default()
                }
                .as_slice(),
            );
            let lookup = source.request(fuse::Opcode::Lookup, 1, b"data\0");
            let inode = u64::from_ne_bytes(lookup[..8].try_into().unwrap());
            let opened = source.request(
                fuse::Opcode::Open,
                inode,
                fuse::OpenIn::default().as_slice(),
            );
            let handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
            assert_eq!(source.read(inode, handle), b"3456");
            let state = source.freeze();
            let mut ram = vec![0; 0x40000];
            source.mem.read_slice(&mut ram, GuestAddress(0)).unwrap();
            std::fs::write(directory.join("ram"), ram).unwrap();
            std::fs::write(
                directory.join("state.json"),
                serde_json::to_vec(&(state, source.next, inode, handle)).unwrap(),
            )
            .unwrap();
        } else {
            assert_eq!(role, "restore");
            let (state, next, inode, handle): (DeviceSnapshot, u16, u64, u64) =
                serde_json::from_slice(&std::fs::read(directory.join("state.json")).unwrap())
                    .unwrap();
            let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
            mem.write_slice(
                &std::fs::read(directory.join("ram")).unwrap(),
                GuestAddress(0),
            )
            .unwrap();
            let mut target = GuestFs::new(&root, Some((mem, state, next)));
            // No INIT, LOOKUP or OPEN in the new process.
            assert_eq!(target.read(inode, handle), b"3456");
        }
        std::fs::write(
            directory.join(format!("{role}.pid")),
            std::process::id().to_string(),
        )
        .unwrap();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("root")).unwrap();
    std::fs::write(directory.path().join("root/data"), b"0123456789").unwrap();
    for role in ["save", "restore"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "fs_restore_in_new_process_keeps_the_old_guest_handle",
                "--nocapture",
            ])
            .env(ROLE, role)
            .env(DIRECTORY, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{role}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_ne!(
        std::fs::read(directory.path().join("save.pid")).unwrap(),
        std::fs::read(directory.path().join("restore.pid")).unwrap()
    );
}

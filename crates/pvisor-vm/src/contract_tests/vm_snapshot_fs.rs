//! Exercise the real virtio-fs worker through guest descriptor rings.
#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[cfg(target_os = "linux")]
use crate::devices::legacy::DummyIrqChip;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::devices::legacy::{GicV3, VcpuList};
use crate::devices::{
    legacy::IrqChipDevice,
    virtio::{
        fs::{fuse, passthrough::PermissionSemantics, Fs},
        DeviceQueue, DeviceSnapshot, InterruptTransport, Queue, VirtioDevice,
    },
};
use crate::utils::eventfd::{EventFd, EFD_NONBLOCK};
use std::{
    sync::{atomic::AtomicI32, Arc, Mutex},
    time::{Duration, Instant},
};
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemoryMmap};

fn interrupt_chip() -> crate::devices::legacy::IrqChip {
    #[cfg(target_os = "macos")]
    let inner = GicV3::new(Arc::new(VcpuList::new(1)));
    #[cfg(target_os = "linux")]
    let inner = DummyIrqChip::new();
    Arc::new(Mutex::new(IrqChipDevice::new(Box::new(inner))))
}

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
        let chip = interrupt_chip();
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
        assert_eq!(response.unique, input.unique, "stale FUSE response");
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

#[test]
fn batched_requests_survive_freeze_thaw_without_another_guest_kick() {
    let root = tempfile::tempdir().unwrap();
    let mut source = GuestFs::new(root.path(), None);
    let gate = crate::devices::virtio::memory_gate::register(&source.mem);
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
    let start = source.next;
    const COUNT: u16 = 16;
    for index in 0..COUNT {
        let input_addr = 0x20000 + u64::from(index) * 0x100;
        let output_addr = 0x28000 + u64::from(index) * 0x100;
        let payload = fuse::OpenIn::default();
        let header = fuse::InHeader {
            len: (std::mem::size_of::<fuse::InHeader>() + payload.as_slice().len()) as u32,
            opcode: fuse::Opcode::Opendir as u32,
            nodeid: 1,
            unique: u64::from(start + index) + 1,
            pid: 1,
            ..Default::default()
        };
        source
            .mem
            .write_slice(header.as_slice(), GuestAddress(input_addr))
            .unwrap();
        source
            .mem
            .write_slice(
                payload.as_slice(),
                GuestAddress(input_addr + header.as_slice().len() as u64),
            )
            .unwrap();
        for (descriptor, addr, len, flags, next) in [
            (index * 2, input_addr, header.len, 1u16, index * 2 + 1),
            (index * 2 + 1, output_addr, 0x100, 2, 0),
        ] {
            let base = 0x8000 + u64::from(descriptor) * 16;
            source.mem.write_obj(addr, GuestAddress(base)).unwrap();
            source.mem.write_obj(len, GuestAddress(base + 8)).unwrap();
            source
                .mem
                .write_obj(flags, GuestAddress(base + 12))
                .unwrap();
            source.mem.write_obj(next, GuestAddress(base + 14)).unwrap();
        }
        source
            .mem
            .write_obj(
                index * 2,
                GuestAddress(0xc004 + u64::from((start + index) % 1024) * 2),
            )
            .unwrap();
    }
    source.next += COUNT;
    source
        .mem
        .write_obj(source.next, GuestAddress(0xc002))
        .unwrap();
    source.events[1].write(1).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while source.mem.read_obj::<u16>(GuestAddress(0xd002)).unwrap() == start {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let _snapshot = source.freeze();
    assert!(
        gate.try_close().unwrap(),
        "freeze retained an in-flight RAM lease"
    );
    gate.open();
    source.fs.thaw().unwrap(); // intentionally no new eventfd kick
    while source.mem.read_obj::<u16>(GuestAddress(0xd002)).unwrap() != source.next {
        assert!(Instant::now() < deadline, "thaw lost available descriptors");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut used = std::collections::BTreeSet::new();
    for index in 0..COUNT {
        let response: fuse::OutHeader = source
            .mem
            .read_obj(GuestAddress(0x28000 + u64::from(index) * 0x100))
            .unwrap();
        assert_eq!(response.error, 0);
        assert_eq!(response.unique, u64::from(start + index) + 1);
        let head = source
            .mem
            .read_obj::<u32>(GuestAddress(0xd004 + u64::from(start + index) * 8))
            .unwrap();
        assert!(used.insert(head), "duplicate completion");
    }
    assert_eq!(used, (0..COUNT).map(|i| u32::from(i * 2)).collect());
    source.fs.reset();
    assert!(
        gate.try_close().unwrap(),
        "reset retained a filesystem worker"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unlinked_cached_inode_rejection_identifies_absence_of_application_handles() {
    // Characterize a remaining capture limitation: an ordinary LOOKUP/UNLINK
    // leaves an O_PATH cache pin until the guest sends FORGET. There is no open
    // application handle, but snapshot capture currently rejects the cache pin.
    // This is a deterministic reproducer for investigation, not a claim that
    // POSIX closed/unlinked inode caching must remain unsupported.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("atomic"), b"old").unwrap();
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
    source.request(fuse::Opcode::Lookup, 1, b"atomic\0");
    source.request(fuse::Opcode::Unlink, 1, b"atomic\0");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !source.fs.freeze().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let error = source.fs.capture_state().unwrap_err();
    assert!(error.contains("unlinked inode"), "{error}");
    assert!(error.contains("application handle: false"), "{error}");
    source.fs.thaw().unwrap();
    std::fs::write(root.path().join("replacement"), b"new").unwrap();
    source.request(fuse::Opcode::Lookup, 1, b"replacement\0");
}
impl Drop for GuestFs {
    fn drop(&mut self) {
        self.fs.reset();
    }
}

#[test]
fn frozen_backing_verification_preserves_writable_handles_and_rejects_changed_originals() {
    use crate::devices::virtio::DeviceSnapshotState;
    for fault in ["content", "inode", "root"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("original");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("data"), b"frozen writable handle").unwrap();
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
        source.request(
            fuse::Opcode::Open,
            inode,
            fuse::OpenIn {
                flags: libc::O_RDWR as u32,
                ..Default::default()
            }
            .as_slice(),
        );
        let mut state = source.freeze();
        let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
            panic!()
        };
        let pristine = serde_json::to_vec(server).unwrap();
        server.verify_frozen_backing().unwrap();
        assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
        assert!(server.rebind_owned_copy(&root, &root).is_err());
        assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
        match fault {
            "content" => std::fs::write(root.join("data"), b"changed frozen content").unwrap(),
            "inode" => {
                std::fs::rename(root.join("data"), temp.path().join("old-data")).unwrap();
                std::fs::write(root.join("data"), b"frozen writable handle").unwrap();
            }
            "root" => {
                std::fs::rename(&root, temp.path().join("old-root")).unwrap();
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("data"), b"frozen writable handle").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(server.verify_frozen_backing().is_err(), "{fault}");
        assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    }
}

#[test]
fn overlay_rebinds_owned_layers_handles_cookies_and_future_hard_link_copy_up() {
    use crate::devices::virtio::{fs::OverlayConfig, DeviceSnapshotState};
    use pvisor::environment_snapshot::{copy_owned_tree, verify_tree};
    use std::{os::unix::fs::MetadataExt, path::Path};

    let workspace = tempfile::tempdir().unwrap();
    let original = workspace.path().join("original");
    for name in ["base", "toolkit", "target"] {
        std::fs::create_dir_all(original.join(name)).unwrap();
    }
    std::fs::write(original.join("base/a"), b"0123456789").unwrap();
    for alias in ["b", "c"] {
        std::fs::hard_link(original.join("base/a"), original.join("base").join(alias)).unwrap();
    }
    std::fs::write(original.join("base/priority"), b"base").unwrap();
    std::fs::write(original.join("toolkit/priority"), b"toolkit").unwrap();
    std::fs::write(original.join("target/unvisited"), b"owned archive entry").unwrap();
    let original = original.canonicalize().unwrap();
    let config = |root: &Path| OverlayConfig {
        lower_mutability: Vec::new(),
        lower_dirs: vec![
            root.join("toolkit").to_str().unwrap().into(),
            root.join("base").to_str().unwrap().into(),
        ],
        apply_target: Some(root.join("target").to_str().unwrap().into()),
        baseline_lower: Some(root.join("base").to_str().unwrap().into()),
        baseline_content_index: None,
        upper_dir: root.join("upper").to_str().unwrap().into(),
        work_dir: Some(root.join("work").to_str().unwrap().into()),
        preimage_dir: Some(root.join("preimages").to_str().unwrap().into()),
        excluded_paths: vec![],
        access_policy: Default::default(),
        semantics: PermissionSemantics::LinuxComplete,
    };
    let make = |root: &Path| {
        Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            None,
            Arc::new(AtomicI32::new(0)),
            false,
            vec![],
            Some(config(root)),
        )
        .unwrap()
    };
    let mut source = GuestFs::from_device(make(&original), None);
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
    let entry = source.request(fuse::Opcode::Lookup, 1, b"a\0");
    let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    let opened = source.request(
        fuse::Opcode::Open,
        inode,
        fuse::OpenIn {
            flags: libc::O_RDWR as u32,
            ..Default::default()
        }
        .as_slice(),
    );
    let handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let mut rename = fuse::RenameIn { newdir: 1 }.as_slice().to_vec();
    rename.extend_from_slice(b"a\0a-moved\0");
    source.request(fuse::Opcode::Rename, 1, &rename);
    let entry = source.request(fuse::Opcode::Lookup, 1, b"priority\0");
    let priority_inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    let opened = source.request(
        fuse::Opcode::Open,
        priority_inode,
        fuse::OpenIn::default().as_slice(),
    );
    let priority_handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let opened = source.request(fuse::Opcode::Opendir, 1, fuse::OpenIn::default().as_slice());
    let directory = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let read_directory = fuse::ReadIn {
        fh: directory,
        size: 4096,
        ..Default::default()
    };
    let listing = source.request(fuse::Opcode::Readdir, 1, read_directory.as_slice());
    let first = fuse::Dirent::from_slice(&listing[..std::mem::size_of::<fuse::Dirent>()]).unwrap();
    let read_remainder = fuse::ReadIn {
        offset: first.off,
        ..read_directory
    };
    let remainder = source.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice());
    let mut state = source.freeze();
    {
        let DeviceSnapshotState::Fs { server, .. } = &state.state else {
            panic!()
        };
        let before = serde_json::to_vec(server).unwrap();
        server.verify_frozen_backing().unwrap();
        assert_eq!(serde_json::to_vec(server).unwrap(), before);
    }
    let copied = workspace.path().join("copied");
    let inventory = copy_owned_tree(&original, &copied).unwrap();
    // Rebinding stores canonical owned roots. macOS temporary paths can use
    // /var aliases for /private/var; construct the restored device identically.
    let copied = copied.canonicalize().unwrap();
    verify_tree(&copied, &inventory).unwrap();
    let damaged = workspace.path().join("damaged");
    copy_owned_tree(&original, &damaged).unwrap();
    let mem = source.mem.clone();
    let next = source.next;
    drop(source);
    std::fs::remove_dir_all(&original).unwrap();

    let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
        panic!()
    };
    let pristine = serde_json::to_vec(server).unwrap();
    let changed =
        pvisor_overlay_core::FileAccessPolicy::new(vec!["private/**".into()], vec![]).unwrap();
    assert!(server.rebind_overlay_policy(&changed).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    let mut rebound = server.clone();
    let mut policy = pvisor_overlay_core::FileAccessPolicy::default();
    policy.bind_session("restored-run", "restored-attempt", "rootfs");
    rebound.rebind_overlay_policy(&policy).unwrap();
    let rebound = serde_json::to_value(&rebound).unwrap();
    let rebound_policy: pvisor_overlay_core::FileAccessPolicy = serde_json::from_value(
        rebound["fs"]["state"]["inner"]["state"]["config"]["access_policy"].clone(),
    )
    .unwrap();
    assert!(rebound_policy.same_rules(&pvisor_overlay_core::FileAccessPolicy::default()));
    assert_eq!(rebound_policy.context().unwrap().run_id, "restored-run");
    assert_eq!(
        rebound_policy.context().unwrap().attempt_id,
        "restored-attempt"
    );
    assert!(server.rebind_owned_copy(&copied, &copied).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    // A failure in a later layer must roll back earlier layer relocation too.
    std::fs::write(damaged.join("base/a"), b"XXXXXXXXXX").unwrap();
    assert!(verify_tree(&damaged, &inventory).is_err());
    std::fs::write(damaged.join("toolkit/priority"), b"XXXXXXXXXX").unwrap();
    assert!(server.rebind_owned_copy(&original, &damaged).is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    // Legacy snapshots may restore against unchanged backing, but cannot fork
    // copied hard-link groups without the original lower identities' paths.
    let mut legacy = serde_json::to_value(&*server).unwrap();
    legacy["fs"]["state"]["inner"]["state"]
        .as_object_mut()
        .unwrap()
        .remove("hard_link_origins")
        .unwrap();
    let mut legacy: crate::devices::virtio::fs::snapshot::ServerSnapshot =
        serde_json::from_value(legacy).unwrap();
    assert!(legacy.rebind_owned_copy(&original, &copied).is_err());

    for corruption in [
        "external-layer",
        "origin-path",
        "origin-layer",
        "duplicate-origin",
    ] {
        let mut malformed = serde_json::to_value(&*server).unwrap();
        let overlay = &mut malformed["fs"]["state"]["inner"]["state"];
        match corruption {
            "external-layer" => {
                overlay["config"]["lower_dirs"][0] =
                    workspace.path().join("outside").to_str().unwrap().into();
            }
            "origin-path" => {
                overlay["hard_link_origins"][0][3] = serde_json::json!(b"../outside".to_vec());
            }
            "origin-layer" => overlay["hard_link_origins"][0][2] = 0.into(),
            "duplicate-origin" => {
                let origin = overlay["hard_link_origins"][0].clone();
                overlay["hard_link_origins"]
                    .as_array_mut()
                    .unwrap()
                    .push(origin);
            }
            _ => unreachable!(),
        }
        let mut malformed: crate::devices::virtio::fs::snapshot::ServerSnapshot =
            serde_json::from_value(malformed).unwrap();
        let before = serde_json::to_vec(&malformed).unwrap();
        assert!(
            malformed.rebind_owned_copy(&original, &copied).is_err(),
            "{corruption}"
        );
        assert_eq!(serde_json::to_vec(&malformed).unwrap(), before);
    }

    let copies: Vec<_> = ["toolkit", "base", "upper", "work", "preimages", "target"]
        .into_iter()
        .map(|name| (original.join(name), copied.join(name)))
        .collect();
    for shared in [
        vec![original.join("upper")],
        vec![original.join("work")],
        vec![original.join("preimages")],
        vec![original.join("target")],
        vec![original.join("base")], // Also the private baseline.
        vec![original.join("missing")],
        vec![original.join("toolkit"), original.join("toolkit")],
    ] {
        assert!(server
            .rebind_shared_readonly_layers(&copies, &shared)
            .is_err());
        assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    }
    for flags in [libc::O_RDWR, libc::O_WRONLY, libc::O_RDONLY | libc::O_TRUNC] {
        let mut malformed = serde_json::to_value(&*server).unwrap();
        fn writable_lower(state: &mut serde_json::Value, flags: i32) {
            match state["kind"].as_str().unwrap() {
                "ReadOnly" => writable_lower(&mut state["state"], flags),
                "Augment" => writable_lower(&mut state["state"]["inner"], flags),
                "Passthrough" => {
                    assert!(!state["state"]["handles"].as_array().unwrap().is_empty());
                    state["state"]["handles"][0]["flags"] = flags.into();
                }
                other => panic!("unexpected lower {other}"),
            }
        }
        writable_lower(
            &mut malformed["fs"]["state"]["inner"]["state"]["layers"][1],
            flags,
        );
        let mut malformed: crate::devices::virtio::fs::snapshot::ServerSnapshot =
            serde_json::from_value(malformed).unwrap();
        let before = serde_json::to_vec(&malformed).unwrap();
        assert!(malformed
            .rebind_shared_readonly_layers(&copies, &[original.join("toolkit")])
            .is_err());
        assert_eq!(serde_json::to_vec(&malformed).unwrap(), before);
    }
    let mut shared = server.clone();
    shared
        .rebind_shared_readonly_layers(&copies, &[original.join("toolkit")])
        .unwrap();

    server.rebind_owned_copy(&original, &copied).unwrap();
    let mut restored = GuestFs::from_device(make(&copied), Some((mem, state, next)));
    // No new INIT, LOOKUP or OPEN for the old file/directory handles.
    assert_eq!(restored.read(inode, handle), b"3456");
    assert_eq!(restored.read(priority_inode, priority_handle), b"lkit");
    assert_eq!(
        restored.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice()),
        remainder
    );
    for alias in [b"b\0", b"c\0"] {
        let entry = restored.request(fuse::Opcode::Lookup, 1, alias);
        let alias_inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
        restored.request(
            fuse::Opcode::Open,
            alias_inode,
            fuse::OpenIn {
                flags: libc::O_RDWR as u32,
                ..Default::default()
            }
            .as_slice(),
        );
    }
    let identity = |name| {
        let stat = std::fs::metadata(copied.join("upper").join(name)).unwrap();
        (stat.dev(), stat.ino())
    };
    assert_eq!(identity("a-moved"), identity("b"));
    assert_eq!(identity("a-moved"), identity("c"));
    assert_ne!(
        identity("a-moved").1,
        std::fs::metadata(copied.join("base/a")).unwrap().ino()
    );
    let mut write = fuse::WriteIn {
        fh: handle,
        size: 3,
        ..Default::default()
    }
    .as_slice()
    .to_vec();
    write.extend_from_slice(b"new");
    restored.request(fuse::Opcode::Write, inode, &write);
    assert_eq!(
        std::fs::read(copied.join("upper/b")).unwrap(),
        b"new3456789"
    );
    assert_eq!(std::fs::read(copied.join("base/a")).unwrap(), b"0123456789");
    // The relocated origin map remains valid for a second capture/fork.
    let mut state = restored.freeze();
    let second = workspace.path().join("second");
    copy_owned_tree(&copied, &second).unwrap();
    let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
        panic!()
    };
    server.rebind_owned_copy(&copied, &second).unwrap();
}

#[test]
fn shared_lower_restores_keep_handles_cookies_and_hard_link_copy_up_private() {
    use crate::devices::virtio::{fs::OverlayConfig, DeviceSnapshotState};
    use pvisor::environment_snapshot::copy_owned_tree;
    use std::{
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        path::Path,
    };

    // Snapshot source bindings use canonical paths, including on macOS.
    let workspace = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let original = workspace.path().join("original");
    std::fs::create_dir_all(original.join("lower")).unwrap();
    std::fs::write(original.join("lower/a"), b"0123456789").unwrap();
    for alias in ["b", "c"] {
        std::fs::hard_link(original.join("lower/a"), original.join("lower").join(alias)).unwrap();
    }
    std::fs::write(original.join("lower/priority"), b"read-only").unwrap();
    let make = |root: &Path, lower: &Path| {
        Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            None,
            Arc::new(AtomicI32::new(0)),
            false,
            vec![],
            Some(OverlayConfig {
                lower_mutability: Vec::new(),
                lower_dirs: vec![lower.to_str().unwrap().into()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: root.join("upper").to_str().unwrap().into(),
                work_dir: Some(root.join("work").to_str().unwrap().into()),
                preimage_dir: Some(root.join("preimages").to_str().unwrap().into()),
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            }),
        )
        .unwrap()
    };
    let mut source = GuestFs::from_device(make(&original, &original.join("lower")), None);
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
    let mut open = |name: &[u8], flags: i32| {
        let entry = source.request(fuse::Opcode::Lookup, 1, name);
        let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
        let opened = source.request(
            fuse::Opcode::Open,
            inode,
            fuse::OpenIn {
                flags: flags as u32,
                ..Default::default()
            }
            .as_slice(),
        );
        (inode, u64::from_ne_bytes(opened[..8].try_into().unwrap()))
    };
    let (inode, handle) = open(b"a\0", libc::O_RDWR);
    let (priority_inode, priority_handle) = open(b"priority\0", libc::O_RDONLY);
    let opened = source.request(fuse::Opcode::Opendir, 1, fuse::OpenIn::default().as_slice());
    let directory = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let read_directory = fuse::ReadIn {
        fh: directory,
        size: 4096,
        ..Default::default()
    };
    let listing = source.request(fuse::Opcode::Readdir, 1, read_directory.as_slice());
    let first = fuse::Dirent::from_slice(&listing[..std::mem::size_of::<fuse::Dirent>()]).unwrap();
    let read_remainder = fuse::ReadIn {
        offset: first.off,
        ..read_directory
    };
    let remainder = source.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice());
    let state = source.freeze();
    let mut memory = vec![0; 0x40000];
    source.mem.read_slice(&mut memory, GuestAddress(0)).unwrap();
    let next = source.next;
    let lower = workspace.path().join("shared-lower");
    copy_owned_tree(&original.join("lower"), &lower).unwrap();
    // The fresh runner can retain original read-only roots during freeze. Its
    // supervisor then binds only those roots to sealed pool copies, preserving
    // the captured upper/work/journal inode identities and saved guest handles.
    let mut partial = state.clone();
    let DeviceSnapshotState::Fs { server, .. } = &mut partial.state else {
        panic!()
    };
    let before = serde_json::to_value(&*server).unwrap();
    for private in ["upper", "work", "preimages"] {
        assert!(server
            .rebind_shared_lower_copies(&[(original.join(private), lower.clone())])
            .is_err());
        assert_eq!(serde_json::to_value(&*server).unwrap(), before);
    }
    for private in ["upper", "work", "preimages"] {
        assert!(server
            .rebind_shared_lower_copies(&[(original.join("lower"), original.join(private))])
            .is_err());
        assert_eq!(serde_json::to_value(&*server).unwrap(), before);
    }
    assert!(server
        .rebind_shared_lower_copies(&[(original.join("lower"), workspace.path().to_owned())])
        .is_err());
    assert_eq!(serde_json::to_value(&*server).unwrap(), before);
    server
        .rebind_shared_lower_copies(&[(original.join("lower"), lower.clone())])
        .unwrap();
    let original_upper_inode = std::fs::metadata(original.join("upper/a")).unwrap().ino();
    let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
    mem.write_slice(&memory, GuestAddress(0)).unwrap();
    let mut partially_rebound =
        GuestFs::from_device(make(&original, &lower), Some((mem, partial, next)));
    assert_eq!(partially_rebound.read(inode, handle), b"3456");
    assert_eq!(
        partially_rebound.read(priority_inode, priority_handle),
        b"d-on"
    );
    assert_eq!(
        partially_rebound.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice()),
        remainder
    );
    assert_eq!(
        std::fs::metadata(original.join("upper/a")).unwrap().ino(),
        original_upper_inode
    );
    drop(partially_rebound);
    let branches: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let root = workspace.path().join(name);
            std::fs::create_dir(&root).unwrap();
            for private in ["upper", "work", "preimages"] {
                copy_owned_tree(&original.join(private), &root.join(private)).unwrap();
            }
            root
        })
        .collect();
    drop(source);
    std::fs::remove_dir_all(&original).unwrap();
    for (root, contents) in branches.iter().zip([b"one", b"two"]) {
        let mut state = state.clone();
        let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
            panic!()
        };
        let copies: Vec<_> = std::iter::once((original.join("lower"), lower.clone()))
            .chain(
                ["upper", "work", "preimages"]
                    .into_iter()
                    .map(|name| (original.join(name), root.join(name))),
            )
            .collect();
        server
            .rebind_shared_readonly_layers(&copies, &[original.join("lower")])
            .unwrap();
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
        mem.write_slice(&memory, GuestAddress(0)).unwrap();
        let mut restored = GuestFs::from_device(make(root, &lower), Some((mem, state, next)));
        // Retained descriptors and directory cookies need no new INIT/OPEN.
        assert_eq!(restored.read(inode, handle), b"3456");
        assert_eq!(restored.read(priority_inode, priority_handle), b"d-on");
        assert_eq!(
            restored.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice()),
            remainder
        );
        for alias in [b"b\0", b"c\0"] {
            let entry = restored.request(fuse::Opcode::Lookup, 1, alias);
            let alias_inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
            restored.request(
                fuse::Opcode::Open,
                alias_inode,
                fuse::OpenIn {
                    flags: libc::O_RDWR as u32,
                    ..Default::default()
                }
                .as_slice(),
            );
        }
        let mut write = fuse::WriteIn {
            fh: handle,
            size: 3,
            ..Default::default()
        }
        .as_slice()
        .to_vec();
        write.extend_from_slice(contents);
        restored.request(fuse::Opcode::Write, inode, &write);
        let identity = |name| {
            std::fs::metadata(root.join("upper").join(name))
                .unwrap()
                .ino()
        };
        assert_eq!(identity("a"), identity("b"));
        assert_eq!(identity("a"), identity("c"));
        assert_ne!(
            identity("a"),
            std::fs::metadata(lower.join("a")).unwrap().ino()
        );
        assert_eq!(
            std::fs::read(root.join("upper/b")).unwrap(),
            [contents.as_slice(), b"3456789"].concat()
        );
        // Recapture retains the same authenticated, immutable lower while
        // upper/work/journal are independently copied. The owned-copy API
        // still rejects unchanged roots, even when they contain valid data.
        let mut recaptured = restored.freeze();
        let mut ram = vec![0; 0x40000];
        restored.mem.read_slice(&mut ram, GuestAddress(0)).unwrap();
        let next = restored.next;
        let child = root.with_extension("recaptured");
        std::fs::create_dir(&child).unwrap();
        for private in ["upper", "work", "preimages"] {
            copy_owned_tree(&root.join(private), &child.join(private)).unwrap();
        }
        let copies: Vec<_> = std::iter::once((lower.clone(), lower.clone()))
            .chain(
                ["upper", "work", "preimages"]
                    .into_iter()
                    .map(|name| (root.join(name), child.join(name))),
            )
            .collect();
        let DeviceSnapshotState::Fs { server, .. } = &mut recaptured.state else {
            panic!()
        };
        let before = serde_json::to_vec(server).unwrap();
        assert!(server.rebind_owned_layers(&copies).is_err());
        assert_eq!(serde_json::to_vec(server).unwrap(), before);
        let mut malformed = serde_json::to_value(&*server).unwrap();
        fn corrupt_identity(state: &mut serde_json::Value, root: &[u8]) -> bool {
            match state["kind"].as_str().unwrap() {
                "ReadOnly" => corrupt_identity(&mut state["state"], root),
                "Augment" => corrupt_identity(&mut state["state"]["inner"], root),
                "Overlay" => state["state"]["layers"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .any(|layer| corrupt_identity(layer, root)),
                "Passthrough" if state["state"]["root"] == serde_json::json!(root) => {
                    let inode = &mut state["state"]["inodes"][0]["identity"]["ino"];
                    *inode = (inode.as_u64().unwrap() + 1).into();
                    true
                }
                "Passthrough" => false,
                other => panic!("unexpected snapshot layer {other}"),
            }
        }
        assert!(corrupt_identity(
            &mut malformed["fs"],
            lower.as_os_str().as_bytes()
        ));
        let mut malformed: crate::devices::virtio::fs::snapshot::ServerSnapshot =
            serde_json::from_value(malformed).unwrap();
        let malformed_before = serde_json::to_vec(&malformed).unwrap();
        assert!(malformed
            .rebind_shared_readonly_layers(&copies, std::slice::from_ref(&lower))
            .is_err());
        assert_eq!(serde_json::to_vec(&malformed).unwrap(), malformed_before);
        server
            .rebind_shared_readonly_layers(&copies, std::slice::from_ref(&lower))
            .unwrap();
        let lower_inode = std::fs::metadata(lower.join("a")).unwrap().ino();
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
        mem.write_slice(&ram, GuestAddress(0)).unwrap();
        let mut child_vm =
            GuestFs::from_device(make(&child, &lower), Some((mem, recaptured, next)));
        assert_eq!(child_vm.read(priority_inode, priority_handle), b"d-on");
        assert_eq!(child_vm.read(inode, handle), b"3456");
        assert_eq!(
            child_vm.request(fuse::Opcode::Readdir, 1, read_remainder.as_slice()),
            remainder
        );
        assert_eq!(
            std::fs::metadata(lower.join("a")).unwrap().ino(),
            lower_inode
        );
        let mut write = fuse::WriteIn {
            fh: handle,
            size: 3,
            ..Default::default()
        }
        .as_slice()
        .to_vec();
        write.extend_from_slice(b"new");
        child_vm.request(fuse::Opcode::Write, inode, &write);
        assert_eq!(std::fs::read(child.join("upper/b")).unwrap(), b"new3456789");
        assert_eq!(
            std::fs::read(root.join("upper/b")).unwrap(),
            [contents.as_slice(), b"3456789"].concat()
        );
    }
    assert_eq!(
        std::fs::read(branches[0].join("upper/a")).unwrap(),
        b"one3456789"
    );
    assert_eq!(
        std::fs::read(branches[1].join("upper/a")).unwrap(),
        b"two3456789"
    );
    assert_ne!(
        std::fs::metadata(branches[0].join("upper/a"))
            .unwrap()
            .ino(),
        std::fs::metadata(branches[1].join("upper/a"))
            .unwrap()
            .ino(),
    );
    assert_eq!(std::fs::read(lower.join("a")).unwrap(), b"0123456789");
    assert_eq!(std::fs::metadata(lower.join("a")).unwrap().nlink(), 3);
}

#[test]
fn fs_rebinds_verified_copy_after_original_tree_is_removed() {
    use crate::devices::virtio::DeviceSnapshotState;
    use std::os::unix::fs::{symlink, MetadataExt};

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
    assert!(std::process::Command::new("/bin/cp")
        .args(["-pR"])
        .arg(&root)
        .arg(&copied)
        .status()
        .unwrap()
        .success());
    assert_ne!(
        std::fs::metadata(root.join("data")).unwrap().ino(),
        std::fs::metadata(copied.join("data")).unwrap().ino()
    );
    let damaged = workspace.path().join("damaged");
    assert!(std::process::Command::new("/bin/cp")
        .arg("-pR")
        .arg(&root)
        .arg(&damaged)
        .status()
        .unwrap()
        .success());
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
                value["state"]["state"]["server"]["fs"]["state"]["inner"]["state"]["inodes"][1]
                    ["path"] = serde_json::json!(b"../outside".to_vec())
            }
            "allocator" => value["state"]["state"]["server"]["next_inode"] = 1.into(),
            "flags" => {
                value["state"]["state"]["server"]["fs"]["state"]["inner"]["state"]["handles"][0]
                    ["flags"] = 0x200.into()
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
        let chip = interrupt_chip();
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
    let chip = interrupt_chip();
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
    assert!(fresh
        .activate(destination.mem.clone(), interrupt, queues)
        .is_err());
}

#[test]
fn overlay_preserves_directory_cookies_and_consumed_virtual_names() {
    use crate::devices::virtio::fs::{
        virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent},
        OverlayConfig,
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
                    content: VirtualEntryContent::File {
                        data: std::sync::Arc::from(b"static".as_slice()),
                    },
                },
            }],
            Some(OverlayConfig {
                lower_mutability: Vec::new(),
                lower_dirs: vec![lower.path().to_str().unwrap().into()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
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
    let crate::devices::virtio::DeviceSnapshotState::Fs { server, .. } = state.state else {
        panic!()
    };
    let value = serde_json::to_value(server).unwrap();
    let names = value["fs"]["state"]["names"].as_array().unwrap();
    assert!(names.is_empty());
    assert!(value["fs"]["state"]["inodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i[0] == virtual_inode));
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
                "contract_tests::vm_snapshot_fs::fs_restore_in_new_process_keeps_the_old_guest_handle",
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

#[test]
fn virtiofs_content_open_preserves_target_preimage_across_restore_and_composed_lowers() {
    use crate::devices::virtio::fs::OverlayConfig;
    use pvisor_overlay_core::apply::{apply_overlay, OverlayRecord, OverlayState, OverlayUpper};
    for frozen in [false, true] {
        for restore in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("target");
            let baseline = temp.path().join("baseline");
            let top = temp.path().join("top");
            let stage = temp.path().join("stage");
            std::fs::create_dir(&target).unwrap();
            std::fs::create_dir(&top).unwrap();
            std::fs::write(target.join("value"), b"original target").unwrap();
            std::fs::write(top.join("value"), b"visible extra layer").unwrap();
            let baseline_lower = frozen.then(|| {
                std::fs::create_dir(&baseline).unwrap();
                std::fs::copy(target.join("value"), baseline.join("value")).unwrap();
                baseline.clone()
            });
            let config = OverlayConfig {
                lower_mutability: Vec::new(),
                lower_dirs: vec![
                    top.to_str().unwrap().into(),
                    baseline_lower
                        .as_ref()
                        .unwrap_or(&target)
                        .to_str()
                        .unwrap()
                        .into(),
                ],
                apply_target: Some(target.to_str().unwrap().into()),
                baseline_lower: baseline_lower.as_ref().map(|p| p.to_str().unwrap().into()),
                baseline_content_index: None,
                upper_dir: stage.join("upper").to_str().unwrap().into(),
                work_dir: Some(stage.join("work").to_str().unwrap().into()),
                preimage_dir: Some(stage.join("preimages").to_str().unwrap().into()),
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            };
            let make = || {
                Fs::new(
                    "rootfs".into(),
                    PermissionSemantics::LinuxComplete,
                    None,
                    Arc::new(AtomicI32::new(0)),
                    false,
                    vec![],
                    Some(config.clone()),
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
            let entry = source.request(fuse::Opcode::Lookup, 1, b"value\0");
            let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
            assert!(
                pvisor_overlay_core::load_preimages(&stage.join("preimages"))
                    .unwrap()
                    .is_empty()
            );
            let opened = source.request(
                fuse::Opcode::Open,
                inode,
                fuse::OpenIn::default().as_slice(),
            );
            let read_handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
            assert_eq!(
                source.request(
                    fuse::Opcode::Read,
                    inode,
                    fuse::ReadIn {
                        fh: read_handle,
                        size: 64,
                        ..Default::default()
                    }
                    .as_slice()
                ),
                b"visible extra layer"
            );
            if restore {
                let state = source.freeze();
                let mem = source.mem.clone();
                let next = source.next;
                drop(source);
                source = GuestFs::from_device(make(), Some((mem, state, next)));
            }
            std::fs::write(target.join("value"), b"host edit").unwrap();
            let opened = source.request(
                fuse::Opcode::Open,
                inode,
                fuse::OpenIn {
                    flags: (libc::O_WRONLY | libc::O_TRUNC) as u32,
                    ..Default::default()
                }
                .as_slice(),
            );
            let write_handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
            let mut payload = fuse::WriteIn {
                fh: write_handle,
                size: 10,
                ..Default::default()
            }
            .as_slice()
            .to_vec();
            payload.extend_from_slice(b"agent edit");
            source.request(fuse::Opcode::Write, inode, &payload);
            drop(source);
            let mut record = OverlayRecord {
                id: "virtiofs-read".into(),
                generation: 0,
                target: target.clone(),
                baseline_lower,
                upper: OverlayUpper {
                    upper_dir: stage.join("upper"),
                    work_dir: stage.join("work"),
                },
                merged_dir: stage.join("merged"),
                stage_dir: stage,
                excluded_paths: vec![],
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            };
            let error = apply_overlay(&mut record).unwrap_err();
            assert!(
                error.to_string().contains("target changed after staging"),
                "{error}"
            );
            assert_eq!(std::fs::read(target.join("value")).unwrap(), b"host edit");
        }
    }
}

#[test]
fn overlay_stage_retains_lower_and_restores_open_handles_and_future_alias_copy_up() {
    use crate::devices::virtio::{fs::OverlayConfig, DeviceSnapshotState};
    use pvisor::environment_snapshot::copy_owned_tree;
    use std::{os::unix::fs::MetadataExt, path::Path};
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("base");
    std::fs::create_dir(&base).unwrap();
    std::fs::write(base.join("a"), b"0123456789").unwrap();
    std::fs::hard_link(base.join("a"), base.join("b")).unwrap();
    std::fs::write(base.join("gone"), b"whiteout me").unwrap();
    let base = base.canonicalize().unwrap();
    let content_index = temp.path().join("content-index.bin");
    let receipts = ["a", "b", "gone"].map(|name| {
        (
            std::path::PathBuf::from(name),
            pvisor::environment_snapshot::file_hash(&base.join(name)).unwrap(),
        )
    });
    let bytes = pvisor_overlay_core::encode_content_index(
        receipts
            .iter()
            .map(|(path, digest)| (path.as_path(), digest.as_str())),
    )
    .unwrap();
    std::fs::write(&content_index, bytes).unwrap();
    let index_sha256 = pvisor::environment_snapshot::file_hash(&content_index).unwrap();
    let stage = temp.path().join("stage");
    std::fs::create_dir(&stage).unwrap();
    let stage = stage.canonicalize().unwrap();
    let make = |stage: &Path| {
        Fs::new(
            "rootfs".into(),
            PermissionSemantics::LinuxComplete,
            None,
            Arc::new(AtomicI32::new(0)),
            false,
            vec![],
            Some(OverlayConfig {
                lower_mutability: Vec::new(),
                lower_dirs: vec![base.to_str().unwrap().into()],
                upper_dir: stage.join("upper").to_str().unwrap().into(),
                work_dir: Some(stage.join("work").to_str().unwrap().into()),
                preimage_dir: Some(stage.join("preimages").to_str().unwrap().into()),
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: Some(crate::api::BaselineContentIndex {
                    root: base.clone(),
                    file: content_index.clone(),
                    sha256: index_sha256.clone(),
                }),
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            }),
        )
        .unwrap()
    };
    let mut guest = GuestFs::from_device(make(&stage), None);
    guest.request(
        fuse::Opcode::Init,
        0,
        fuse::InitInCompat {
            major: 7,
            minor: 31,
            ..Default::default()
        }
        .as_slice(),
    );
    let entry = guest.request(fuse::Opcode::Lookup, 1, b"a\0");
    let inode = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    let opened = guest.request(
        fuse::Opcode::Open,
        inode,
        fuse::OpenIn {
            flags: libc::O_RDWR as u32,
            ..Default::default()
        }
        .as_slice(),
    );
    let handle = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let mut rename = fuse::RenameIn { newdir: 1 }.as_slice().to_vec();
    rename.extend_from_slice(b"a\0a-moved\0");
    guest.request(fuse::Opcode::Rename, 1, &rename);
    guest.request(fuse::Opcode::Unlink, 1, b"gone\0");
    let mut state = guest.freeze();
    let copied = temp.path().join("branch");
    copy_owned_tree(&stage, &copied).unwrap();
    let copied = copied.canonicalize().unwrap();
    let mem = guest.mem.clone();
    let next = guest.next;
    drop(guest);
    std::fs::remove_dir_all(&stage).unwrap();
    let copies = ["upper", "work", "preimages"].map(|part| (stage.join(part), copied.join(part)));
    let DeviceSnapshotState::Fs { server, .. } = &mut state.state else {
        panic!()
    };
    let pristine = serde_json::to_vec(server).unwrap();
    let mut writable_base = serde_json::to_value(&*server).unwrap();
    writable_base["fs"]["state"]["inner"]["state"]["config"]["apply_target"] =
        base.to_str().unwrap().into();
    let mut writable_base: crate::devices::virtio::fs::snapshot::ServerSnapshot =
        serde_json::from_value(writable_base).unwrap();
    assert!(writable_base
        .rebind_stage(&copies, std::slice::from_ref(&base))
        .is_err());

    assert!(server.rebind_owned_layers(&copies).is_err());
    assert!(server
        .rebind_stage(&copies, &[copied.join("upper")])
        .is_err());
    assert!(server
        .rebind_stage(&copies[..1], std::slice::from_ref(&base))
        .is_err());
    assert_eq!(serde_json::to_vec(server).unwrap(), pristine);
    server
        .rebind_stage(&copies, std::slice::from_ref(&base))
        .unwrap();
    let mut restored = GuestFs::from_device(make(&copied), Some((mem, state, next)));
    assert_eq!(restored.read(inode, handle), b"3456");
    // A lower alias never opened by the source joins its copied-up upper inode.
    let entry = restored.request(fuse::Opcode::Lookup, 1, b"b\0");
    let alias = u64::from_ne_bytes(entry[..8].try_into().unwrap());
    restored.request(
        fuse::Opcode::Open,
        alias,
        fuse::OpenIn {
            flags: libc::O_RDWR as u32,
            ..Default::default()
        }
        .as_slice(),
    );
    assert_eq!(
        std::fs::metadata(copied.join("upper/a-moved"))
            .unwrap()
            .ino(),
        std::fs::metadata(copied.join("upper/b")).unwrap().ino()
    );
    let mut write = fuse::WriteIn {
        fh: handle,
        size: 3,
        ..Default::default()
    }
    .as_slice()
    .to_vec();
    write.extend_from_slice(b"new");
    restored.request(fuse::Opcode::Write, inode, &write);
    assert_eq!(
        std::fs::read(copied.join("upper/b")).unwrap(),
        b"new3456789"
    );
    assert_eq!(std::fs::read(base.join("a")).unwrap(), b"0123456789");
    assert_eq!(std::fs::read(base.join("gone")).unwrap(), b"whiteout me");
    // Readdir proves the saved deletion remains hidden after restore.
    let opened = restored.request(fuse::Opcode::Opendir, 1, fuse::OpenIn::default().as_slice());
    let directory = u64::from_ne_bytes(opened[..8].try_into().unwrap());
    let listing = restored.request(
        fuse::Opcode::Readdir,
        1,
        fuse::ReadIn {
            fh: directory,
            size: 4096,
            ..Default::default()
        }
        .as_slice(),
    );
    assert!(!listing.windows(4).any(|part| part == b"gone"));
}

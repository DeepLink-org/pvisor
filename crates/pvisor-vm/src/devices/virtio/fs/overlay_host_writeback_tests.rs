// Included inside overlay::tests so these contracts can inspect private owners.

fn host_buffer_config(root: &Path) -> Config {
    std::fs::create_dir_all(root.join("lower")).unwrap();
    Config {
        lower_dirs: vec![root.join("lower").to_string_lossy().into_owned()],
        lower_mutability: Vec::new(),
        upper_dir: root.join("upper").to_string_lossy().into_owned(),
        work_dir: None,
        preimage_dir: None,
        apply_target: None,
        baseline_lower: None,
        baseline_content_index: None,
        excluded_paths: Vec::new(),
        access_policy: Default::default(),
        semantics: passthrough::PermissionSemantics::LinuxComplete,
    }
}

#[cfg(target_os = "linux")]
fn host_buffer_fixture(root: &Path) -> OverlayFs {
    let mut fs = OverlayFs::build_with_writeback(
        host_buffer_config(root),
        Arc::new(InodeAllocator::new()),
        false,
        Some("0"),
    )
    .unwrap();
    fs.configure_host_writeback(true, false).unwrap();
    let options = fs
        .init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
        .unwrap();
    assert!(!options.intersects(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK));
    fs
}

#[cfg(target_os = "linux")]
fn host_buffer_context() -> Context {
    Context {
        uid: 0,
        gid: 0,
        pid: 1,
    }
}

#[cfg(target_os = "linux")]
fn host_buffer_open_file(fs: &OverlayFs) -> (u64, u64) {
    let ctx = host_buffer_context();
    let (entry, handle, _) = fs
        .create(
            ctx,
            fuse::ROOT_ID,
            c"file",
            0o600,
            false,
            libc::O_RDWR as u32,
            0,
            Extensions::default(),
        )
        .unwrap();
    (entry.inode, handle.unwrap())
}

#[cfg(target_os = "linux")]
fn host_buffer_write(
    fs: &OverlayFs,
    inode: u64,
    handle: u64,
    bytes: &[u8],
    offset: u64,
) -> io::Result<usize> {
    fs.write(
        host_buffer_context(),
        inode,
        handle,
        WriteReader(io::Cursor::new(bytes.to_vec())),
        bytes.len() as u32,
        offset,
        None,
        false,
        false,
        0,
    )
}

#[cfg(target_os = "linux")]
struct HostBufferGate(Arc<(Mutex<bool>, Condvar)>);

#[cfg(target_os = "linux")]
impl HostBufferGate {
    fn release(&self) {
        let (state, ready) = &*self.0;
        *state.lock().unwrap() = true;
        ready.notify_all();
    }
}

#[cfg(target_os = "linux")]
impl Drop for HostBufferGate {
    fn drop(&mut self) {
        // A failing assertion must release the worker before scoped threads join.
        self.release();
    }
}

#[cfg(target_os = "linux")]
struct HostBufferBlockedTarget {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: std::sync::mpsc::Sender<()>,
    error: Option<i32>,
}

#[cfg(target_os = "linux")]
impl BufferedWriteTarget for HostBufferBlockedTarget {
    fn merge_key(&self) -> (u64, bool) {
        (u64::MAX, false)
    }

    fn write(&self, chunks: &[&[u8]], _offset: u64) -> io::Result<usize> {
        let _ = self.entered.send(());
        let (state, ready) = &*self.gate;
        let mut released = state.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
        match self.error {
            Some(error) => Err(io::Error::from_raw_os_error(error)),
            None => Ok(chunks.iter().map(|chunk| chunk.len()).sum()),
        }
    }
}

#[cfg(target_os = "linux")]
fn host_buffer_block(fs: &OverlayFs, error: Option<i32>) -> HostBufferGate {
    let gate = HostBufferGate(Arc::new((Mutex::new(false), Condvar::new())));
    let (entered, waiting) = std::sync::mpsc::channel();
    fs.buffered_writes()
        .unwrap()
        .reserve(1)
        .unwrap()
        .submit(
            Box::new(HostBufferBlockedTarget {
                gate: gate.0.clone(),
                entered,
                error,
            }),
            vec![0],
            0,
        )
        .unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    gate
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_protocol_write_detaches_guest_ram_before_backing_completion() {
    use crate::devices::virtio::descriptor_utils::{
        create_descriptor_chain, DescriptorType, OwnedDescriptorChain,
    };
    use crate::devices::virtio::memory_gate;
    use std::mem::size_of;
    use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

    for failure in [None, Some(libc::ENOSPC)] {
        let temp = tempfile::tempdir().unwrap();
        let fs = host_buffer_fixture(temp.path());
        let (inode, handle) = host_buffer_open_file(&fs);
        let buffer = fs.buffered_writes().unwrap();
        // Tuple fields drop in order: unblock before the server joins its worker.
        let session = (
            host_buffer_block(&fs, failure),
            super::super::super::server::Server::new(fs, true),
        );
        let (backing, server) = &session;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let ram = memory_gate::register(&mem);
        let payload = b"host-owned";
        let input = 0x1000;
        let body_len = size_of::<fuse::WriteIn>() + payload.len();
        let reply = GuestAddress(input + body_len as u64);
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(input),
            vec![
                (DescriptorType::Readable, body_len as u32),
                (DescriptorType::Writable, 128),
            ],
            0,
        )
        .unwrap();
        mem.write_obj(
            fuse::WriteIn {
                fh: handle,
                size: payload.len() as u32,
                ..Default::default()
            },
            GuestAddress(input),
        )
        .unwrap();
        let data = GuestAddress(input + size_of::<fuse::WriteIn>() as u64);
        mem.write_slice(payload, data).unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        let (reader, writer) = plan.reader_writer(&mem).unwrap();
        let reply_len = server
            .handle_message(
                (
                    fuse::InHeader {
                        len: (size_of::<fuse::InHeader>() + body_len) as u32,
                        opcode: fuse::Opcode::Write as u32,
                        nodeid: inode,
                        unique: 99,
                        ..Default::default()
                    },
                    reader,
                ),
                writer,
                false,
                &None,
                &Arc::new(std::sync::atomic::AtomicI32::new(i32::MAX)),
            )
            .unwrap();
        assert_eq!(
            reply_len,
            size_of::<fuse::OutHeader>() + size_of::<fuse::WriteOut>()
        );
        let header: fuse::OutHeader = mem.read_obj(reply).unwrap();
        let written: fuse::WriteOut = mem
            .read_obj(GuestAddress(reply.0 + size_of::<fuse::OutHeader>() as u64))
            .unwrap();
        assert_eq!(
            (header.error, header.unique, written.size),
            (0, 99, payload.len() as u32)
        );
        assert!(std::fs::read(temp.path().join("upper/file"))
            .unwrap()
            .is_empty());
        assert!(!ram.try_close().unwrap());
        drop(plan);
        mem.write_slice(b"reused!!!!", data).unwrap();
        assert!(ram.try_close().unwrap());

        backing.release();
        assert_eq!(
            buffer.flush().err().and_then(|error| error.raw_os_error()),
            failure
        );
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            payload
        );
        assert!(ram.is_idle_closed());
        let after: fuse::OutHeader = mem.read_obj(reply).unwrap();
        assert_eq!((after.error, after.unique), (header.error, header.unique));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_ordinary_write_ack_precedes_backing_io_and_read_waits_for_owned_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let fs = host_buffer_fixture(temp.path());
    let (inode, handle) = host_buffer_open_file(&fs);
    let backing = temp.path().join("upper/file");
    std::thread::scope(|scope| {
        let gate = host_buffer_block(&fs, None);
        let (acknowledged, ack) = std::sync::mpsc::channel();
        let fs_ref = &fs;
        scope.spawn(move || {
            acknowledged
                .send(host_buffer_write(fs_ref, inode, handle, b"host-owned", 0))
                .unwrap();
        });
        assert_eq!(
            ack.recv_timeout(Duration::from_secs(2)).unwrap().unwrap(),
            10
        );
        assert!(std::fs::read(&backing).unwrap().is_empty());
        let (started, entering) = std::sync::mpsc::channel();
        let (completed, received) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let mut writer = ReadWriter {
                gate: None,
                bytes: Vec::new(),
            };
            started.send(()).unwrap();
            let result = fs_ref.read(
                host_buffer_context(),
                inode,
                handle,
                &mut writer,
                32,
                0,
                None,
                0,
            );
            completed.send(result.map(|_| writer.bytes)).unwrap();
        });
        entering.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        gate.release();
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            b"host-owned"
        );
    });
    fs.release(host_buffer_context(), inode, 0, handle, false, false, None)
        .unwrap();
    assert!(fs.io_activity.state.lock().unwrap().active.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_aliases_preserve_write_order_and_read_metadata_open_visibility() {
    let temp = tempfile::tempdir().unwrap();
    let fs = host_buffer_fixture(temp.path());
    let ctx = host_buffer_context();
    let (inode, handle) = host_buffer_open_file(&fs);
    let alias = fs.link(ctx, inode, fuse::ROOT_ID, c"alias").unwrap().inode;
    let second = fs
        .open(ctx, alias, false, libc::O_RDWR as u32)
        .unwrap()
        .0
        .unwrap();
    std::thread::scope(|scope| {
        let gate = host_buffer_block(&fs, None);
        assert_eq!(
            host_buffer_write(&fs, inode, handle, b"abcdefgh", 0).unwrap(),
            8
        );
        assert_eq!(host_buffer_write(&fs, alias, second, b"XY", 2).unwrap(), 2);
        let (started, entering) = std::sync::mpsc::channel();
        let (completed, received) = std::sync::mpsc::channel();
        for operation in 0..3 {
            let fs = &fs;
            let backing = temp.path().join("upper/alias");
            let started = started.clone();
            let completed = completed.clone();
            scope.spawn(move || {
                started.send(()).unwrap();
                let observed = (|| -> io::Result<(usize, Vec<u8>)> {
                    match operation {
                        0 => {
                            let mut writer = ReadWriter {
                                gate: None,
                                bytes: Vec::new(),
                            };
                            let count = fs.read(ctx, alias, second, &mut writer, 32, 0, None, 0)?;
                            Ok((count, writer.bytes))
                        }
                        1 => {
                            let size = fs.getattr(ctx, alias, None)?.0.st_size as usize;
                            Ok((size, std::fs::read(backing)?))
                        }
                        _ => {
                            let readonly = fs
                                .open(ctx, alias, false, libc::O_RDONLY as u32)?
                                .0
                                .unwrap();
                            // Read the backing directly: another adapter barrier must not
                            // accidentally conceal a stale read-only OPEN result.
                            let bytes = std::fs::read(backing)?;
                            fs.release(ctx, alias, 0, readonly, false, false, None)?;
                            Ok((bytes.len(), bytes))
                        }
                    }
                })();
                completed.send(observed).unwrap();
            });
        }
        for _ in 0..3 {
            entering.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        gate.release();
        for _ in 0..3 {
            let (count, bytes) = received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(count, 8);
            assert_eq!(bytes, b"abXYefgh");
        }
    });
    fs.release(ctx, inode, 0, handle, false, false, None)
        .unwrap();
    fs.release(ctx, alias, 0, second, false, false, None)
        .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_flush_sync_and_release_wait_for_accepted_writes() {
    for operation in 0..5 {
        let temp = tempfile::tempdir().unwrap();
        let fs = host_buffer_fixture(temp.path());
        let (inode, handle) = host_buffer_open_file(&fs);
        std::thread::scope(|scope| {
            let gate = host_buffer_block(&fs, None);
            host_buffer_write(&fs, inode, handle, b"complete", 0).unwrap();
            let (started, entering) = std::sync::mpsc::channel();
            let (completed, received) = std::sync::mpsc::channel();
            let fs = &fs;
            scope.spawn(move || {
                let ctx = host_buffer_context();
                started.send(()).unwrap();
                let result = match operation {
                    0 => fs.flush(ctx, inode, handle, 0),
                    1 => fs.fsync(ctx, inode, false, handle),
                    2 => fs.fsync(ctx, inode, true, handle),
                    3 => fs.syncfs(ctx),
                    _ => fs.release(ctx, inode, 0, handle, false, false, None),
                };
                completed.send(result).unwrap();
            });
            entering.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(
                received.recv_timeout(Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            gate.release();
            received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
        });
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            b"complete"
        );
        if operation != 4 {
            fs.release(host_buffer_context(), inode, 0, handle, false, false, None)
                .unwrap();
        }
        assert!(!fs.handles.lock().unwrap().contains_key(&handle));
        assert!(fs.io_activity.state.lock().unwrap().active.is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_async_error_is_sticky_and_release_still_removes_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let fs = host_buffer_fixture(temp.path());
    let ctx = host_buffer_context();
    let (inode, handle) = host_buffer_open_file(&fs);
    let before_error = fs.capture_state().unwrap();
    {
        let gate = host_buffer_block(&fs, Some(libc::ENOSPC));
        host_buffer_write(&fs, inode, handle, b"accepted", 0).unwrap();
        gate.release();
    }
    for result in [
        fs.flush(ctx, inode, handle, 0),
        fs.fsync(ctx, inode, false, handle),
        fs.fsync(ctx, inode, true, handle),
        fs.fsyncdir(ctx, fuse::ROOT_ID, false, 0),
        fs.syncfs(ctx),
        fs.capture_state().map(|_| ()),
        fs.restore_state(&before_error),
    ] {
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOSPC));
    }
    assert_eq!(
        host_buffer_write(&fs, inode, handle, b"retry", 0)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOSPC)
    );
    assert_eq!(
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOSPC)
    );
    assert!(!fs.handles.lock().unwrap().contains_key(&handle));
    assert!(fs.io_activity.state.lock().unwrap().active.is_empty());
    assert_eq!(
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EBADF)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_snapshot_drains_then_restore_and_new_init_have_empty_queues() {
    let temp = tempfile::tempdir().unwrap();
    let fs = host_buffer_fixture(temp.path());
    let (inode, handle) = host_buffer_open_file(&fs);
    let saved = std::thread::scope(|scope| {
        let gate = host_buffer_block(&fs, None);
        host_buffer_write(&fs, inode, handle, b"snapshot", 0).unwrap();
        let (started, entering) = std::sync::mpsc::channel();
        let (completed, received) = std::sync::mpsc::channel();
        let fs = &fs;
        scope.spawn(move || {
            started.send(()).unwrap();
            completed.send(fs.capture_state()).unwrap();
        });
        entering.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        gate.release();
        received
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap()
    });
    saved.validate_writeback(false).unwrap();
    let mut restored = OverlayFs::build_with_writeback(
        fs.snapshot_config.clone(),
        Arc::new(InodeAllocator::new()),
        true,
        Some("0"),
    )
    .unwrap();
    restored.configure_host_writeback(true, false).unwrap();
    restored.restore_state(&saved).unwrap();
    let mut writer = ReadWriter {
        gate: None,
        bytes: Vec::new(),
    };
    restored
        .read(
            host_buffer_context(),
            inode,
            handle,
            &mut writer,
            32,
            0,
            None,
            0,
        )
        .unwrap();
    assert_eq!(writer.bytes, b"snapshot");
    fs.release(host_buffer_context(), inode, 0, handle, false, false, None)
        .unwrap();
    let previous = restored.buffered_writes().unwrap();
    restored.destroy();
    assert!(restored.buffered_writes().is_none());
    assert!(previous.reserve(1).is_err());
    restored.init(FsOptions::empty()).unwrap();
    assert!(!Arc::ptr_eq(
        &previous,
        &restored.buffered_writes().unwrap()
    ));
    let inode = restored
        .lookup(host_buffer_context(), fuse::ROOT_ID, c"file")
        .unwrap()
        .inode;
    let handle = restored
        .open(host_buffer_context(), inode, false, libc::O_RDWR as u32)
        .unwrap()
        .0
        .unwrap();
    host_buffer_write(&restored, inode, handle, b"new", 0).unwrap();
    restored
        .flush(host_buffer_context(), inode, handle, 0)
        .unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("upper/file")).unwrap(),
        b"newpshot"
    );
    restored
        .release(host_buffer_context(), inode, 0, handle, false, false, None)
        .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_custom_options_survive_destroy_and_new_init() {
    let temp = tempfile::tempdir().unwrap();
    let mut fs = OverlayFs::build_with_writeback(
        host_buffer_config(temp.path()),
        Arc::new(InodeAllocator::new()),
        false,
        Some("0"),
    )
    .unwrap();
    let options = WriteBufferOptions {
        budget_bytes: 4 * 1024 * 1024,
        max_jobs: 3,
        batch_bytes: 4096,
        max_chunks: 1,
        max_delay: Duration::ZERO,
    };
    fs.configure_host_writeback_options(true, false, options, true, false)
        .unwrap();
    for session in 0..2 {
        let negotiated = fs
            .init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        assert!(!negotiated.intersects(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK));
        assert_eq!(fs.host_writeback_options.budget_bytes, options.budget_bytes);
        assert_eq!(fs.host_writeback_options.max_jobs, options.max_jobs);
        assert_eq!(fs.host_writeback_options.batch_bytes, options.batch_bytes);
        assert_eq!(fs.host_writeback_options.max_chunks, options.max_chunks);
        assert_eq!(fs.host_writeback_options.max_delay, options.max_delay);
        assert!(fs.host_writeback_qualification_lock);
        assert!(!fs.host_writeback_synchronous);
        let buffer = fs.buffered_writes().unwrap();
        assert_eq!(
            buffer
                .reserve(options.budget_bytes + 1)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let (inode, handle) = if session == 0 {
            host_buffer_open_file(&fs)
        } else {
            let ctx = host_buffer_context();
            let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
            let handle = fs
                .open(ctx, inode, false, libc::O_RDWR as u32)
                .unwrap()
                .0
                .unwrap();
            (inode, handle)
        };
        let gate = host_buffer_block(&fs, None);
        host_buffer_write(&fs, inode, handle, b"aa", 0).unwrap();
        host_buffer_write(&fs, inode, handle, b"bb", 2).unwrap();
        // One blocking control target plus two accepted native writes. The
        // custom one-chunk limit must apply after both INITs, rather than
        // silently returning to default coalescing after DESTROY.
        assert_eq!(buffer.stats().peak_jobs, 3);
        gate.release();
        fs.flush(host_buffer_context(), inode, handle, 0).unwrap();
        assert_eq!((buffer.stats().total_jobs, buffer.stats().batches), (3, 3));
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            b"aabb"
        );
        fs.release(host_buffer_context(), inode, 0, handle, false, false, None)
            .unwrap();
        fs.destroy();
        assert!(fs.buffered_writes().is_none());
        assert_eq!(
            buffer.reserve(1).err().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
fn host_buffer_configuration_rejects_dax_platform_and_guest_writeback_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let mut fs = OverlayFs::build_with_writeback(
        host_buffer_config(temp.path()),
        Arc::new(InodeAllocator::new()),
        false,
        Some("0"),
    )
    .unwrap();
    assert!(fs.buffered_writes().is_none());
    fs.configure_host_writeback(false, true).unwrap();
    assert_eq!(
        fs.configure_host_writeback(true, true).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(fs.buffered_writes().is_none());
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        fs.configure_host_writeback(true, false).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    #[cfg(target_os = "linux")]
    {
        fs.configure_host_writeback(true, false).unwrap();
        assert!(!fs
            .init(FsOptions::WRITEBACK_CACHE)
            .unwrap()
            .contains(FsOptions::WRITEBACK_CACHE));
    }
    let guest = tempfile::tempdir().unwrap();
    let config = host_buffer_config(guest.path());
    let mut source = OverlayFs::build_with_writeback(
        config.clone(),
        Arc::new(InodeAllocator::new()),
        false,
        Some("1"),
    )
    .unwrap();
    assert_eq!(
        source
            .configure_host_writeback(true, false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    #[cfg(target_os = "linux")]
    {
        assert!(source
            .init(FsOptions::WRITEBACK_CACHE)
            .unwrap()
            .contains(FsOptions::WRITEBACK_CACHE));
        let saved = source.capture_state().unwrap();
        let mut restored = OverlayFs::build_with_writeback(
            config,
            Arc::new(InodeAllocator::new()),
            true,
            Some("0"),
        )
        .unwrap();
        restored.configure_host_writeback(true, false).unwrap();
        assert_eq!(
            restored.restore_state(&saved).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!restored.writeback_active.load(Ordering::Relaxed));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn host_buffer_short_guest_read_refunds_admission_without_enqueuing_partial_data() {
    let temp = tempfile::tempdir().unwrap();
    let fs = host_buffer_fixture(temp.path());
    let (inode, handle) = host_buffer_open_file(&fs);
    {
        let gate = host_buffer_block(&fs, None);
        let result = fs.write(
            host_buffer_context(),
            inode,
            handle,
            WriteReader(io::Cursor::new(b"bad".to_vec())),
            8,
            0,
            None,
            false,
            false,
            0,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert!(fs.io_activity.state.lock().unwrap().active.is_empty());
        assert!(std::fs::read(temp.path().join("upper/file"))
            .unwrap()
            .is_empty());
        gate.release();
    }
    fs.flush(host_buffer_context(), inode, handle, 0).unwrap();
    assert!(std::fs::read(temp.path().join("upper/file"))
        .unwrap()
        .is_empty());
    host_buffer_write(&fs, inode, handle, b"valid", 0).unwrap();
    fs.fsync(host_buffer_context(), inode, false, handle)
        .unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("upper/file")).unwrap(),
        b"valid"
    );
    fs.release(host_buffer_context(), inode, 0, handle, false, false, None)
        .unwrap();
}

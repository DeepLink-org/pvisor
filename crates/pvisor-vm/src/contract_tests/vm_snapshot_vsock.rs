//! Frozen vsock worker ownership and empty-connection snapshots, without a Linux/HVF VM.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::devices::{
    legacy::{GicV3, IrqChipDevice, VcpuList},
    virtio::{
        vsock::{TsiFlags, Vsock},
        DeviceQueue, DeviceSnapshot, InterruptTransport, Queue, VirtioDevice,
    },
};
use crate::utils::eventfd::{EventFd, EFD_NONBLOCK};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn mem() -> GuestMemoryMmap {
    GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20000)]).unwrap()
}
fn queue(q: u64) -> Queue {
    let mut queue = Queue::new(256);
    queue.size = 256;
    queue.ready = true;
    queue.desc_table = GuestAddress(base(q));
    queue.avail_ring = GuestAddress(base(q) + 0x1000);
    queue.used_ring = GuestAddress(base(q) + 0x2000);
    queue
}
fn base(q: u64) -> u64 {
    0x1000 + q * 0x3000
}
fn used(mem: &GuestMemoryMmap, q: u64) -> u16 {
    mem.read_obj(GuestAddress(base(q) + 0x2002)).unwrap()
}
fn submit(mem: &GuestMemoryMmap, q: u64, index: u16, bytes: &[u8], writable: bool) {
    let data = 0x10000 + q * 0x2000 + u64::from(index) * 256;
    mem.write_slice(bytes, GuestAddress(data)).unwrap();
    let desc = base(q) + u64::from(index) * 16;
    mem.write_obj(data, GuestAddress(desc)).unwrap();
    mem.write_obj(bytes.len() as u32, GuestAddress(desc + 8))
        .unwrap();
    mem.write_obj(if writable { 2u16 } else { 0u16 }, GuestAddress(desc + 12))
        .unwrap();
    mem.write_obj(index, GuestAddress(base(q) + 0x1004 + u64::from(index) * 2))
        .unwrap();
    mem.write_obj(index + 1, GuestAddress(base(q) + 0x1002))
        .unwrap();
}
fn activate(vsock: &mut Vsock, mem: GuestMemoryMmap, saved: Option<&DeviceSnapshot>) {
    if let Some(saved) = saved {
        vsock.restore_state(&saved.state).unwrap();
    }
    // Retain IRQ status but exclude physical injection: no HVF CPU in this fixture.
    let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
        Arc::new(VcpuList::new(1)),
    )))));
    let interrupt = InterruptTransport::new(chip, "vsock-contract".into()).unwrap();
    let queues = (0..3)
        .map(|q| {
            DeviceQueue::new(
                saved.map_or_else(
                    || queue(q),
                    |s| {
                        s.queues.as_ref().unwrap()[q as usize]
                            .restore(256, &mem)
                            .unwrap()
                    },
                ),
                Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
            )
        })
        .collect();
    vsock.activate(mem, interrupt, queues).unwrap();
}
fn new() -> Vsock {
    Vsock::new(3, None, None, TsiFlags::empty()).unwrap()
}
fn freeze(vsock: &mut Vsock) {
    wait(|| vsock.freeze().unwrap());
}

#[test]
fn fresh_workers_stay_parked_until_thaw_and_retain_all_three_queues() {
    let mem = mem();
    let mut source = new();
    activate(&mut source, mem.clone(), None);
    assert!(source.capture_state().is_err());
    freeze(&mut source);
    let saved: DeviceSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&source.capture_state().unwrap()).unwrap())
            .unwrap();
    // Guest descriptors added after capture remain unread while both instances are frozen.
    submit(&mem, 0, 0, &[0xaa; 108], true);
    submit(&mem, 1, 0, &[0; 8], false); // deliberately malformed, completed with len=0 on thaw.
    let mut target = new();
    activate(&mut target, mem.clone(), Some(&saved));
    assert_eq!(
        serde_json::to_value(target.capture_state().unwrap()).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(used(&mem, 0), 0);
    assert_eq!(used(&mem, 1), 0);
    target.thaw().unwrap();
    wait(|| used(&mem, 0) == 1);
    assert!(target.process_stream_tx());
    assert_eq!(used(&mem, 1), 1);
    assert_eq!(mem.read_obj::<u32>(GuestAddress(0x10000 + 24)).unwrap(), 8); // actual payload length
    assert_eq!(
        mem.read_obj::<u32>(GuestAddress(base(0) + 0x2008)).unwrap(),
        52
    ); // hdr44 + timestamp8
    freeze(&mut target);
    assert!(target.capture_state().is_ok());
    // Rejected topology cannot be installed.
    let mut wrong = Vsock::new(4, None, None, TsiFlags::empty()).unwrap();
    assert!(wrong
        .restore_state(&source.capture_state().unwrap().state)
        .is_err());
}

#[test]
fn queued_external_response_rejects_snapshot_and_original_can_resume() {
    let mem = mem();
    let mut source = new();
    activate(&mut source, mem.clone(), None);
    // RW for unknown connection generates an RST response, with no RX buffer to deliver it.
    let mut header = [0u8; 44];
    header[8..16].copy_from_slice(&2u64.to_le_bytes());
    header[16..20].copy_from_slice(&1234u32.to_le_bytes());
    header[20..24].copy_from_slice(&4321u32.to_le_bytes());
    header[28..30].copy_from_slice(&1u16.to_le_bytes());
    header[30..32].copy_from_slice(&5u16.to_le_bytes());
    submit(&mem, 1, 0, &header, false);
    assert!(source.process_stream_tx());
    freeze(&mut source);
    assert!(source
        .capture_state()
        .unwrap_err()
        .contains("pending responses"));
    source.thaw().unwrap();
    submit(&mem, 0, 0, &[0; 52], true);
    // The thawed worker may drain RX before this thread; completion and the
    // actual RST response are the contract, not which thread drains the queue.
    wait(|| {
        source.process_stream_rx();
        used(&mem, 0) == 1
    });
    assert_eq!(used(&mem, 0), 1);
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x10000 + 30)).unwrap(), 3);
    freeze(&mut source);
    assert!(source.capture_state().is_ok());
}

#[test]
fn listeners_reject_snapshot_and_failed_save_does_not_rebind_them() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("listener.sock");
    let mut source = Vsock::new(
        3,
        None,
        Some([(9999, (socket.clone(), true))].into_iter().collect()),
        TsiFlags::empty(),
    )
    .unwrap();
    activate(&mut source, mem(), None);
    wait(|| socket.exists());
    freeze(&mut source);
    assert!(source.capture_state().unwrap_err().contains("listeners"));
    let first = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    source.thaw().unwrap();
    drop(first);
    freeze(&mut source);
    assert!(source.capture_state().is_err());
    // Restart resumes the same worker rather than creating a new listener over a live socket.
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
}

#[test]
fn worker_drop_releases_idle_background_threads() {
    let mut source = new();
    activate(&mut source, mem(), None);
    let start = Instant::now();
    drop(source);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn vsock_snapshot_restarts_in_an_independent_process() {
    const STAGE: &str = "PVISOR_VSOCK_SNAPSHOT_STAGE";
    const DIR: &str = "PVISOR_VSOCK_SNAPSHOT_DIR";
    if let Ok(stage) = std::env::var(STAGE) {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR).unwrap());
        let mem = mem();
        let mut device = new();
        if stage == "save" {
            activate(&mut device, mem.clone(), None);
            freeze(&mut device);
            submit(&mem, 0, 0, &[0; 108], true);
            let state = device.capture_state().unwrap();
            std::fs::write(dir.join("state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
            let mut bytes = vec![0; 0x20000];
            mem.read_slice(&mut bytes, GuestAddress(0)).unwrap();
            std::fs::write(dir.join("ram"), bytes).unwrap();
        } else {
            mem.write_slice(&std::fs::read(dir.join("ram")).unwrap(), GuestAddress(0))
                .unwrap();
            let saved: DeviceSnapshot =
                serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
            activate(&mut device, mem.clone(), Some(&saved));
            assert_eq!(used(&mem, 0), 0);
            assert_eq!(
                serde_json::to_value(device.capture_state().unwrap()).unwrap(),
                serde_json::to_value(saved).unwrap()
            );
            device.thaw().unwrap();
            wait(|| used(&mem, 0) == 1);
            freeze(&mut device);
            assert_eq!(mem.read_obj::<u32>(GuestAddress(0x10000 + 24)).unwrap(), 8);
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for stage in ["save", "restore"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "contract_tests::vm_snapshot_vsock::vsock_snapshot_restarts_in_an_independent_process",
                "--nocapture",
            ])
            .env(STAGE, stage)
            .env(DIR, dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{stage}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

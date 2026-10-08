//! Serialization contracts for macOS VM snapshots.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::devices::{
    legacy::{GicV3, IrqChipDevice, PendingInterrupts, VcpuList},
    virtio::{rng::Rng, MmioSnapshot, MmioTransport, Queue, QueueSnapshot},
    BusDevice,
};
use std::sync::{Arc, Mutex};
use vm_memory::{GuestAddress, GuestMemoryMmap};

#[test]
fn queue_snapshot_checks_complete_ranges_and_keeps_runtime_indices() {
    let mem = GuestMemoryMmap::from_ranges(&[
        (GuestAddress(0x1000), 0x4000),
        (GuestAddress(0x6000), 0x4000),
    ])
    .unwrap();
    let mut queue = Queue::new(256);
    queue.ready = true;
    queue.size = 128;
    queue.desc_table = GuestAddress(0x1000);
    queue.avail_ring = GuestAddress(0x2000);
    queue.used_ring = GuestAddress(0x3000);
    let mut value = serde_json::to_value(queue.capture_state()).unwrap();
    value["next_avail"] = 65535.into();
    value["next_used"] = 7.into();
    value["num_added"] = 11.into();
    value["event_idx_enabled"] = true.into();
    let state: QueueSnapshot = serde_json::from_value(value.clone()).unwrap();
    let restored = state.restore(256, &mem).unwrap();
    assert_eq!(
        serde_json::to_value(restored.capture_state()).unwrap(),
        value
    );
    assert!(state.restore(128, &mem).is_err());
    for addr in [0x5800, 0x4900, 0x1001, u64::MAX - 15] {
        queue.desc_table = GuestAddress(addr);
        assert!(!queue.is_valid(&mem));
        assert!(queue.capture_state().restore(256, &mem).is_err());
    }
    queue.desc_table = GuestAddress(0x4800);
    assert!(queue.is_valid(&mem)); // exclusive end equals region boundary
}

#[test]
fn gic_roundtrip_retains_registers_and_ordered_duplicate_irqs() {
    let source = Arc::new(VcpuList::new(2));
    let gic = GicV3::new(source.clone());
    source
        .restore_pending(&PendingInterrupts {
            queues: vec![vec![27, 9, 9], vec![48]],
        })
        .unwrap();
    let mut state = gic.capture_state().unwrap();
    state.ctlr = 0x53;
    state.waker = 2;
    state.routes[48] = 1;
    state.edge_trigger[1] = 3;
    let target = Arc::new(VcpuList::new(2));
    let mut restored = GicV3::new(target.clone());
    restored.restore_state(&state).unwrap();
    assert_eq!(
        serde_json::to_value(restored.capture_state().unwrap()).unwrap(),
        serde_json::to_value(&state).unwrap()
    );
    use crate::hvf::Vcpus;
    for irq in [27, 9, 9, 1023] {
        assert_eq!(target.get_pending_irq(0), irq);
    }
    assert_eq!(target.get_pending_irq(1), 48);
    let unchanged = serde_json::to_value(restored.capture_state().unwrap()).unwrap();
    state.pending.queues[0] = vec![1023];
    assert!(restored.restore_state(&state).is_err());
    assert_eq!(
        serde_json::to_value(restored.capture_state().unwrap()).unwrap(),
        unchanged
    );
    state.pending.queues = vec![];
    assert!(restored.restore_state(&state).is_err());
}

fn transport(mem: GuestMemoryMmap) -> MmioTransport {
    let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
        Arc::new(VcpuList::new(1)),
    )))));
    let mut device =
        MmioTransport::new(mem, chip, Arc::new(Mutex::new(Rng::new().unwrap()))).unwrap();
    device.set_irq_line(48);
    device
}

#[test]
fn mmio_freeze_retries_busy_backend_without_poisoning_transition() {
    let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0x1000), 0x5000)]).unwrap();
    let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
        Arc::new(VcpuList::new(1)),
    )))));
    let backend = Arc::new(Mutex::new(Rng::new().unwrap()));
    let mut transport = MmioTransport::new(mem, chip, backend.clone()).unwrap();
    let busy = backend.lock().unwrap();
    assert!(!transport.freeze().unwrap());
    drop(busy);
    assert!(transport.freeze().unwrap());
    transport.thaw().unwrap();
}

#[test]
fn mmio_restores_activated_queue_and_rejects_features_before_mutation() {
    let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0x1000), 0x5000)]).unwrap();
    let mut source = transport(mem.clone());
    // Negotiate VERSION_1 then configure one ready RNG queue.
    for status in [1_u32, 3] {
        source.write(0, 0x70, &status.to_le_bytes());
    }
    source.write(0, 0x24, &1_u32.to_le_bytes());
    source.write(0, 0x20, &1_u32.to_le_bytes());
    source.write(0, 0x70, &11_u32.to_le_bytes());
    for (reg, val) in [
        (0x38, 128_u32),
        (0x80, 0x1000),
        (0x90, 0x2000),
        (0xa0, 0x3000),
        (0x44, 1),
    ] {
        source.write(0, reg, &val.to_le_bytes());
    }
    source.write(0, 0x70, &15_u32.to_le_bytes());
    assert!(source.capture_state().is_err());
    assert!(source.freeze().unwrap());
    let value = serde_json::to_value(source.capture_state().unwrap()).unwrap();
    let state: MmioSnapshot = serde_json::from_value(value.clone()).unwrap();
    let mut restored = transport(mem.clone());
    restored.restore_state(&state).unwrap();
    assert_eq!(
        serde_json::to_value(restored.capture_state().unwrap()).unwrap(),
        value
    );
    let mut malformed = value;
    malformed["acked_features"] = u64::MAX.into();
    let invalid: MmioSnapshot = serde_json::from_value(malformed).unwrap();
    let mut fresh = transport(mem);
    let before = serde_json::to_value(fresh.capture_state().unwrap()).unwrap();
    assert!(fresh.restore_state(&invalid).is_err());
    assert_eq!(
        serde_json::to_value(fresh.capture_state().unwrap()).unwrap(),
        before
    );
}

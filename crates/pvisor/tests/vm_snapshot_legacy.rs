//! Legacy-device contracts needed by the full-machine snapshot coordinator.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use devices::{
    Bus, BusDevice,
    legacy::{RTC, RtcSnapshot},
};
use std::sync::{Arc, Mutex};
use utils::eventfd::{EFD_NONBLOCK, EventFd};

fn register(rtc: &mut RTC, offset: u64) -> u32 {
    let mut data = [0; 4];
    rtc.read(0, offset, &mut data);
    u32::from_le_bytes(data)
}

#[test]
fn rtc_restores_loaded_clock_registers_and_pending_event_without_consuming_source() {
    let event = EventFd::new(EFD_NONBLOCK).unwrap();
    let witness = event.try_clone().unwrap();
    let mut source = RTC::new(event);
    source.write(0, 0x8, &123456u32.to_le_bytes());
    source.write(0, 0x4, &456789u32.to_le_bytes());
    source.write(0, 0x10, &1u32.to_le_bytes());
    let state = source.capture_state().unwrap();
    assert_eq!(witness.read().unwrap(), 1);
    let encoded = serde_json::to_string(&state).unwrap();
    let state: RtcSnapshot = serde_json::from_str(&encoded).unwrap();
    let event = EventFd::new(EFD_NONBLOCK).unwrap();
    let witness = event.try_clone().unwrap();
    let mut target = RTC::new(event);
    target.restore_state(&state).unwrap();
    assert_eq!(register(&mut target, 0x8), 123456);
    assert_eq!(register(&mut target, 0x4), 456789);
    assert_eq!(register(&mut target, 0x10), 1);
    assert!(register(&mut target, 0) >= 123456);
    assert_eq!(witness.read().unwrap(), 1);
    assert!(witness.read().is_err());
}

#[test]
fn rtc_rejects_bad_state_before_changing_target() {
    let source = RTC::new(EventFd::new(EFD_NONBLOCK).unwrap());
    let original = serde_json::to_value(source.capture_state().unwrap()).unwrap();
    for (field, value) in [
        ("version", 2),
        ("imsc", 2),
        ("ris", 2),
        ("counter_ns", u64::MAX),
        ("host_monotonic_ns", u64::MAX),
        ("pending_events", u64::MAX),
    ] {
        let mut invalid = original.clone();
        invalid[field] = value.into();
        let state: RtcSnapshot = serde_json::from_value(invalid).unwrap();
        let mut target = RTC::new(EventFd::new(EFD_NONBLOCK).unwrap());
        target.write(0, 0x4, &99u32.to_le_bytes());
        assert!(target.restore_state(&state).is_err(), "{field}");
        assert_eq!(register(&mut target, 0x4), 99);
    }
    let mut unknown = original;
    unknown["unknown"] = true.into();
    assert!(serde_json::from_value::<RtcSnapshot>(unknown).is_err());
}

#[test]
fn rtc_advances_across_suspension_and_wraps_32_bit_counter() {
    let source = RTC::new(EventFd::new(EFD_NONBLOCK).unwrap());
    let mut value = serde_json::to_value(source.capture_state().unwrap()).unwrap();
    // Place the source two seconds before wrap and the anchor five seconds ago.
    // No sleeps or wall-clock changes are needed to exercise downtime semantics.
    value["counter_ns"] = (((1u64 << 32) - 2) * 1_000_000_000).into();
    value["host_monotonic_ns"] = value["host_monotonic_ns"]
        .as_u64()
        .unwrap()
        .checked_sub(5_000_000_000)
        .unwrap()
        .into();
    let state: RtcSnapshot = serde_json::from_value(value).unwrap();
    let mut target = RTC::new(EventFd::new(EFD_NONBLOCK).unwrap());
    target.restore_state(&state).unwrap();
    let counter = register(&mut target, 0);
    assert!((3..=5).contains(&counter), "counter={counter}");
}

#[test]
fn bus_inventory_includes_every_mapping_in_address_order() {
    let mut bus = Bus::new();
    for addr in [0x3000, 0x1000, 0x7000] {
        bus.insert(
            Arc::new(Mutex::new(RTC::new(EventFd::new(EFD_NONBLOCK).unwrap()))),
            addr,
            0x1000,
        )
        .unwrap();
    }
    let ranges: Vec<_> = bus
        .mapped_devices()
        .map(|(base, len, _)| (base, len))
        .collect();
    assert_eq!(
        ranges,
        [(0x1000, 0x1000), (0x3000, 0x1000), (0x7000, 0x1000)]
    );
}

#[test]
fn gpio_roundtrip_preserves_registers_and_requires_matching_topology() {
    use devices::legacy::Gpio;
    let fresh = || {
        Gpio::new(
            EventFd::new(EFD_NONBLOCK).unwrap(),
            EventFd::new(EFD_NONBLOCK).unwrap(),
        )
    };
    let mut source = fresh();
    source.set_irq_line(42);
    source.write(0, 0x400, &0xffu32.to_le_bytes());
    source.write(0, 0x3fc, &0x55u32.to_le_bytes());
    source.write(0, 0x410, &8u32.to_le_bytes());
    assert!(source.capture_state().is_err());
    source.freeze();
    let state = source.capture_state().unwrap();
    let mut target = fresh();
    target.freeze();
    assert!(target.restore_state(&state).is_err());
    target.set_irq_line(42);
    target.restore_state(&state).unwrap();
    assert_eq!(
        serde_json::to_value(target.capture_state().unwrap()).unwrap(),
        serde_json::to_value(state).unwrap()
    );
    target.thaw();
    let mut data = [0; 4];
    target.read(0, 0x400, &mut data);
    assert_eq!(u32::from_le_bytes(data), 0xff);
}

#[test]
fn gpio_shutdown_vetoes_capture_and_is_delivered_on_source_thaw() {
    use devices::legacy::Gpio;
    let shutdown = EventFd::new(EFD_NONBLOCK).unwrap();
    let request = shutdown.try_clone().unwrap();
    let mut source = Gpio::new(shutdown, EventFd::new(EFD_NONBLOCK).unwrap());
    source.freeze();
    request.write(1).unwrap();
    assert!(
        source
            .capture_state()
            .unwrap_err()
            .contains("shutdown pending")
    );
    let mut data = [0; 4];
    source.read(0, 0x414, &mut data);
    assert_eq!(u32::from_le_bytes(data), 0);
    source.thaw();
    source.read(0, 0x414, &mut data);
    assert_eq!(u32::from_le_bytes(data), 8);
}

#[test]
fn full_bus_snapshot_restores_legacy_devices_and_rejects_missing_mapping() {
    use devices::legacy::{Gpio, Serial};
    let make_bus = || {
        let mut bus = Bus::new();
        let mut serial = Serial::new_sink(EventFd::new(EFD_NONBLOCK).unwrap());
        serial.queue_input_bytes(b"abc").unwrap();
        bus.insert(Arc::new(Mutex::new(serial)), 0x1000, 0x1000)
            .unwrap();
        bus.insert(
            Arc::new(Mutex::new(RTC::new(EventFd::new(EFD_NONBLOCK).unwrap()))),
            0x3000,
            0x1000,
        )
        .unwrap();
        bus.insert(
            Arc::new(Mutex::new(Gpio::new(
                EventFd::new(EFD_NONBLOCK).unwrap(),
                EventFd::new(EFD_NONBLOCK).unwrap(),
            ))),
            0x5000,
            0x1000,
        )
        .unwrap();
        bus
    };
    let source = make_bus();
    assert!(source.capture_snapshot_devices().is_err());
    assert!(source.freeze_snapshot_devices().unwrap());
    let states = source.capture_snapshot_devices().unwrap();
    assert_eq!(states.len(), 3);
    let target = make_bus();
    assert!(target.restore_snapshot_devices(&states[..2]).is_err());
    let mut wrong = states.clone();
    wrong[1].device = wrong[0].device.clone();
    assert!(target.restore_snapshot_devices(&wrong).is_err());
    target.restore_snapshot_devices(&states).unwrap();
    target.thaw_snapshot_devices().unwrap();
    for expected in b"abc" {
        let mut data = [0; 4];
        assert!(target.read(0, 0x1000, &mut data));
        assert_eq!(u32::from_le_bytes(data), u32::from(*expected));
    }
    source.thaw_snapshot_devices().unwrap();
}

#[test]
fn unknown_device_vetoes_entire_bus_snapshot() {
    struct Unknown;
    impl BusDevice for Unknown {}
    let mut bus = Bus::new();
    bus.insert(Arc::new(Mutex::new(Unknown)), 0x1000, 0x1000)
        .unwrap();
    assert!(
        bus.freeze_snapshot_devices()
            .unwrap_err()
            .contains("unsupported")
    );
    assert!(bus.capture_snapshot_devices().is_err());
    // No work was started on this device; cancelling freeze remains possible.
    bus.thaw_snapshot_devices().unwrap();
}

#[test]
fn serial_fifo_reset_does_not_leave_uncounted_input_in_snapshot() {
    use devices::legacy::Serial;
    let mut source = Serial::new_sink(EventFd::new(EFD_NONBLOCK).unwrap());
    source.queue_input_bytes(b"old").unwrap();
    source.write(0, 11 * 4, &0x10u32.to_le_bytes());
    source.queue_input_bytes(b"new").unwrap();
    source.freeze();
    let state = source.capture_state().unwrap();
    let mut target = Serial::new_sink(EventFd::new(EFD_NONBLOCK).unwrap());
    target.freeze();
    target.restore_state(&state).unwrap();
    for expected in b"new" {
        let mut data = [0; 4];
        target.read(0, 0, &mut data);
        assert_eq!(u32::from_le_bytes(data), u32::from(*expected));
    }
}

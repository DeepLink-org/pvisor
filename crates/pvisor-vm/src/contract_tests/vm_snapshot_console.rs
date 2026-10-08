//! Real virtqueues and console workers; no claim of whole Linux VM restoration.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::devices::{
    legacy::{GicV3, IrqChipDevice, VcpuList},
    virtio::{
        console::{port_io, Console},
        MmioSnapshot, MmioTransport, PortDescription, VirtioDevice,
    },
    BusDevice,
};
use crate::polly::event_manager::EventManager;
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap, VolatileSlice};

fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < end,
            "console worker did not reach request boundary"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn base(q: u64) -> u64 {
    0x1000 + q * 0x3000
}
fn submit(mem: &GuestMemoryMmap, q: u64, index: u16, bytes: &[u8], writable: bool) {
    let b = base(q);
    let data = b + 0x1800 + u64::from(index) * 64;
    mem.write_slice(bytes, GuestAddress(data)).unwrap();
    let desc = b + u64::from(index) * 16;
    mem.write_obj(data, GuestAddress(desc)).unwrap();
    mem.write_obj(bytes.len() as u32, GuestAddress(desc + 8))
        .unwrap();
    mem.write_obj(if writable { 2u16 } else { 0u16 }, GuestAddress(desc + 12))
        .unwrap();
    mem.write_obj(index, GuestAddress(b + 0x1000 + 4 + u64::from(index) * 2))
        .unwrap();
    mem.write_obj(index + 1, GuestAddress(b + 0x1002)).unwrap();
}
fn used(mem: &GuestMemoryMmap, q: u64) -> u16 {
    mem.read_obj(GuestAddress(base(q) + 0x2002)).unwrap()
}
fn fixture(
    mem: GuestMemoryMmap,
    output: Box<dyn port_io::PortOutput + Send>,
) -> (MmioTransport, EventManager) {
    let console = Arc::new(Mutex::new(
        Console::new(vec![PortDescription {
            name: "stream".into(),
            input: Some(port_io::input_empty().unwrap()),
            output: Some(output),
            terminal: None,
        }])
        .unwrap(),
    ));
    let mut manager = EventManager::new().unwrap();
    manager.add_subscriber(console.clone()).unwrap();
    let chip = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(GicV3::new(
        Arc::new(VcpuList::new(1)),
    )))));
    // No live HVF CPU in this device contract test. IRQ status is retained,
    // while physical CPU interrupt delivery is covered separately.
    let transport = MmioTransport::new(mem, chip, console).unwrap();
    (transport, manager)
}
fn configure(transport: &mut MmioTransport, manager: &mut EventManager) {
    for status in [1u32, 3] {
        transport.write(0, 0x70, &status.to_le_bytes());
    }
    transport.write(0, 0x24, &1u32.to_le_bytes());
    transport.write(0, 0x20, &1u32.to_le_bytes());
    transport.write(0, 0x24, &0u32.to_le_bytes());
    transport.write(0, 0x20, &2u32.to_le_bytes()); // MULTIPORT
    transport.write(0, 0x70, &11u32.to_le_bytes());
    for q in 0..4u32 {
        transport.write(0, 0x30, &q.to_le_bytes());
        for (reg, val) in [
            (0x38, 32),
            (0x80, base(q as u64) as u32),
            (0x90, base(q as u64) as u32 + 0x1000),
            (0xa0, base(q as u64) as u32 + 0x2000),
            (0x44, 1),
        ] {
            transport.write(0, reg, &val.to_le_bytes());
        }
    }
    transport.write(0, 0x70, &15u32.to_le_bytes());
    manager.run_with_timeout(0).unwrap();
}
fn notify(transport: &mut MmioTransport, manager: &mut EventManager, q: u32) {
    transport.write(0, 0x50, &q.to_le_bytes());
    manager.run_with_timeout(10).unwrap();
}
fn freeze(transport: &mut MmioTransport) {
    wait(|| transport.freeze().unwrap());
}

#[test]
fn frozen_workers_resume_old_queues_without_guest_port_open() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source");
    let target_path = dir.path().join("target");
    let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20000)]).unwrap();
    let (mut source, mut events) = fixture(
        mem.clone(),
        port_io::output_file(fs::File::create(&source_path).unwrap()).unwrap(),
    );
    configure(&mut source, &mut events);
    // RX sees EOF, and must retain its completed descriptor and closed state.
    submit(&mem, 0, 0, &[0; 8], true);
    submit(&mem, 3, 0, &[0, 0, 0, 0, 6, 0, 1, 0], false);
    notify(&mut source, &mut events, 3);
    wait(|| used(&mem, 0) == 1);
    submit(&mem, 1, 0, b"before", false);
    notify(&mut source, &mut events, 1);
    wait(|| used(&mem, 1) == 1);
    freeze(&mut source);
    // Repeated PORT_OPEN and invalid port IDs stay pending until thaw.
    submit(&mem, 3, 1, &[0, 0, 0, 0, 6, 0, 1, 0], false);
    submit(&mem, 3, 2, &[99, 0, 0, 0, 6, 0, 1, 0], false);
    submit(&mem, 1, 1, b"after", false);
    notify(&mut source, &mut events, 1);
    assert_eq!(used(&mem, 1), 1);
    let saved: MmioSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&source.capture_state().unwrap()).unwrap())
            .unwrap();
    let (mut target, mut target_events) = fixture(
        mem.clone(),
        port_io::output_file(fs::File::create(&target_path).unwrap()).unwrap(),
    );
    target.restore_state(&saved).unwrap();
    target_events.run_with_timeout(0).unwrap();
    assert_eq!(fs::read(&target_path).unwrap(), b"");
    assert_eq!(
        serde_json::to_value(target.capture_state().unwrap()).unwrap(),
        serde_json::to_value(&saved).unwrap()
    );
    target.thaw().unwrap();
    wait(|| used(&mem, 1) == 2);
    target_events.run_with_timeout(10).unwrap();
    freeze(&mut target);
    assert_eq!(fs::read(&source_path).unwrap(), b"before");
    assert_eq!(fs::read(&target_path).unwrap(), b"after");
    assert_eq!(used(&mem, 0), 1); // EOF does not reopen input on restore.
    assert_eq!(used(&mem, 3), 3); // repeated/open out-of-range commands do not panic.
    assert!(target.capture_state().is_ok());
}

#[test]
fn endpoint_snapshot_keeps_partial_log_and_pending_signal() {
    let mut log = port_io::output_to_log_as_err();
    let mut prefix = b"unfinished".to_vec();
    // SAFETY: owned, live buffer is exclusively borrowed for the duration of this call.
    let slice = unsafe { VolatileSlice::new(prefix.as_mut_ptr(), prefix.len()) };
    log.write_volatile(&slice).unwrap();
    let state = log.capture_state().unwrap();
    let mut restored = port_io::output_to_log_as_err();
    restored.restore_state(&state).unwrap();
    assert_eq!(restored.capture_state().unwrap(), state);
    let bad = port_io::PortIoSnapshot::Log {
        buffer: vec![0; 513],
    };
    assert!(restored.restore_state(&bad).is_err());
    assert!(restored
        .restore_state(&port_io::PortIoSnapshot::NativeFd)
        .is_err());
    assert_eq!(restored.capture_state().unwrap(), state);
    let mut console = Console::new(vec![PortDescription::output_pipe("log", restored)]).unwrap();
    let saved = console.capture_state().unwrap();
    let mut json = serde_json::to_value(&saved.state).unwrap();
    json["state"]["ports"][0]["name"] = "other".into();
    assert!(console
        .restore_state(&serde_json::from_value(json).unwrap())
        .is_err());
    use port_io::PortInput;
    let input = port_io::PortInputSigInt::new();
    input.sigint_evt().write(2).unwrap();
    let state = input.capture_state().unwrap();
    assert_eq!(state, port_io::PortIoSnapshot::Signal { pending: 2 });
    assert_eq!(input.capture_state().unwrap(), state); // capture preserves live source signal.
    let mut target = port_io::PortInputSigInt::new();
    target.restore_state(&state).unwrap();
    assert_eq!(target.capture_state().unwrap(), state);
}

#[test]
fn console_snapshot_crosses_process_boundary() {
    const STAGE: &str = "PVISOR_CONSOLE_SNAPSHOT_STAGE";
    const DIR: &str = "PVISOR_CONSOLE_SNAPSHOT_DIR";
    if let Ok(stage) = std::env::var(STAGE) {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR).unwrap());
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20000)]).unwrap();
        let (mut transport, mut events) = fixture(mem.clone(), port_io::output_to_log_as_err());
        if stage == "save" {
            configure(&mut transport, &mut events);
            submit(&mem, 3, 0, &[0, 0, 0, 0, 6, 0, 1, 0], false);
            notify(&mut transport, &mut events, 3);
            submit(&mem, 1, 0, b"partial-", false);
            notify(&mut transport, &mut events, 1);
            wait(|| used(&mem, 1) == 1);
            freeze(&mut transport);
            submit(&mem, 1, 1, b"continued", false);
            fs::write(
                dir.join("state.json"),
                serde_json::to_vec(&transport.capture_state().unwrap()).unwrap(),
            )
            .unwrap();
            let mut bytes = vec![0; 0x20000];
            mem.read_slice(&mut bytes, GuestAddress(0)).unwrap();
            fs::write(dir.join("ram"), bytes).unwrap();
        } else {
            mem.write_slice(&fs::read(dir.join("ram")).unwrap(), GuestAddress(0))
                .unwrap();
            let saved: MmioSnapshot =
                serde_json::from_slice(&fs::read(dir.join("state.json")).unwrap()).unwrap();
            transport.restore_state(&saved).unwrap();
            events.run_with_timeout(0).unwrap();
            assert_eq!(used(&mem, 1), 1);
            transport.thaw().unwrap();
            wait(|| used(&mem, 1) == 2);
            freeze(&mut transport);
            let state = serde_json::to_value(transport.capture_state().unwrap()).unwrap();
            assert_eq!(
                state["device"]["state"]["state"]["ports"][0]["output"]["Log"]["buffer"],
                serde_json::json!(b"partial-continued".to_vec())
            );
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for stage in ["save", "restore"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "contract_tests::vm_snapshot_console::console_snapshot_crosses_process_boundary",
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

#[test]
fn invalid_control_and_unknown_endpoint_are_rejected() {
    struct Unsupported;
    impl port_io::PortOutput for Unsupported {
        fn write_volatile(&mut self, _: &VolatileSlice) -> std::io::Result<usize> {
            Ok(0)
        }
        fn wait_until_writable(&self) {}
    }
    let unsupported = Console::new(vec![PortDescription::output_pipe(
        "log",
        Box::new(Unsupported),
    )])
    .unwrap();
    assert!(unsupported
        .capture_state()
        .unwrap_err()
        .contains("does not support"));
    let source = Console::new(vec![PortDescription::output_pipe(
        "log",
        port_io::output_to_log_as_err(),
    )])
    .unwrap();
    let mut state = serde_json::to_value(source.capture_state().unwrap().state).unwrap();
    for message in [
        vec![0, 0, 0, 0, 6, 0, 2, 0],
        vec![1, 0, 0, 0, 6, 0, 1, 0],
        vec![0; 7],
        vec![0, 0, 0, 0, 5, 0, 0, 0],
    ] {
        state["state"]["control"] = serde_json::json!([message]);
        let mut target = Console::new(vec![PortDescription::output_pipe(
            "log",
            port_io::output_to_log_as_err(),
        )])
        .unwrap();
        assert!(target
            .restore_state(&serde_json::from_value(state.clone()).unwrap())
            .is_err());
    }
}

#[test]
fn failed_external_output_cannot_be_saved_as_complete() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailedOutput(Arc<AtomicBool>);
    impl port_io::PortOutput for FailedOutput {
        fn write_volatile(&mut self, _: &VolatileSlice) -> std::io::Result<usize> {
            self.0.store(true, Ordering::Release);
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn wait_until_writable(&self) {}
        fn capture_state(&self) -> Result<port_io::PortIoSnapshot, String> {
            Ok(port_io::PortIoSnapshot::NativeFd)
        }
    }
    let attempted = Arc::new(AtomicBool::new(false));
    let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20000)]).unwrap();
    let (mut source, mut events) = fixture(mem.clone(), Box::new(FailedOutput(attempted.clone())));
    configure(&mut source, &mut events);
    submit(&mem, 3, 0, &[0, 0, 0, 0, 6, 0, 1, 0], false);
    notify(&mut source, &mut events, 3);
    submit(&mem, 1, 0, b"cannot-complete", false);
    notify(&mut source, &mut events, 1);
    wait(|| attempted.load(Ordering::Acquire));
    freeze(&mut source);
    assert!(source
        .capture_state()
        .unwrap_err()
        .contains("partial external effects"));
}

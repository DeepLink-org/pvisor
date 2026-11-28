//! KVM exercises the embedded Rust kernel and the public idle lease API.
#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "init-blob"))]

use pvisor_vm::api::{
    GuestIdleControl, RuntimeSupport, VmBuilder, VmConfiguration, VmControl, VmPlatform, VmRuntime,
    VmmHandle,
};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::AsRawFd,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const CHILD: &str = "PVISOR_TEST_NATIVE_IDLE_CHILD";
const DIAGNOSTIC: &str = "PVISOR_CASE_IDLE_DIAGNOSTIC";
const PERIODIC_CONTROL: &str = "periodic";

#[test]
#[ignore = "requires /dev/kvm, embedded Rust native kernel and PVISOR_CASE_IDLE_GUEST"]
fn native_idle_handoff() {
    if let Some(root) = std::env::var_os(CHILD) {
        VmPlatform::init_logging("off");
        run_vm(std::path::Path::new(&root));
        panic!("VM returned without an authenticated exit");
    }
    let probe = std::env::var_os("PVISOR_CASE_IDLE_GUEST").expect("path to idle_handoff.c binary");
    let root = tempfile::tempdir().unwrap();
    for dir in ["dev", "proc", "sys", "tmp"] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    std::fs::copy(probe, root.path().join("probe")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "native_idle_handoff", "--nocapture"])
        .env(CHILD, root.path())
        .env("PVISOR_GUEST_KERNEL", "rust-native")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(7));
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let phase = fs::read_to_string(root.path().join("harness-phase")).unwrap_or_default();
            panic!("timed out waiting for native idle handoff during {phase}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_vm(root: &std::path::Path) {
    let (guest_input, mut host_input) = std::io::pipe().unwrap();
    let (host_output, guest_output) = std::io::pipe().unwrap();

    let mut vm = VmBuilder::new(1, 256).unwrap();
    let diagnostic = std::env::var(DIAGNOSTIC).ok();
    let periodic_control = diagnostic.as_deref() == Some(PERIODIC_CONTROL);
    if !periodic_control {
        vm.guest_idle_handoff().unwrap();
    }
    vm.filesystem("/dev/root", root, 0).unwrap();
    vm.task_stdio(guest_input.as_raw_fd(), guest_output.as_raw_fd(), -1)
        .unwrap();
    let mut argv = vec!["/probe".into()];
    if diagnostic.is_some() {
        argv.push("measure".into());
    }
    let config = pvisor_guest::api::GuestConfig {
        argv,
        cwd: "/".into(),
        stdio_ports: Some([true, true, false]),
        ..Default::default()
    };
    vm.virtual_file(
        "/dev/root",
        pvisor_guest::api::CONFIG_PATH,
        serde_json::to_vec(&config).unwrap(),
        0o400,
        true,
    )
    .unwrap();
    let phase_file = root.join("harness-phase");
    fs::write(&phase_file, "VM setup").unwrap();
    vm.run(|handle| {
        thread::spawn(move || {
            let phase = |name| fs::write(&phase_file, name).unwrap();
            phase("guest BLOCK");
            let mut output = BufReader::new(host_output).lines();
            assert_eq!(output.next().unwrap().unwrap(), "BLOCK");
            phase("idle before arm");
            // Arm after the guest has had time to enter ordinary idle.
            thread::sleep(Duration::from_millis(100));
            let idle_sample = diagnostic.map(|_| {
                let wall = Instant::now();
                let cpu = process_cpu_ns();
                thread::sleep(Duration::from_secs(1));
                (wall.elapsed(), process_cpu_ns() - cpu)
            });
            if periodic_control {
                let (wall, cpu) = idle_sample.unwrap();
                phase("periodic control delivery");
                eprintln!("NATIVE_IDLE_DIAG {} {} -", wall.as_nanos(), cpu);
                host_input.write_all(b"A").unwrap();
                assert_eq!(output.next().unwrap().unwrap(), "DONE");
                return;
            }
            if let Ok(phase) = std::env::var("PVISOR_CASE_IDLE_ARM_PHASE_MS") {
                let phase: u64 = phase.parse().unwrap();
                assert!(phase < 50);
                thread::sleep(Duration::from_millis(phase));
            }
            phase("arm and pause");
            let arm_started = Instant::now();
            let (epoch, remaining) = claim(&handle);
            let arm_to_pause = arm_started.elapsed();
            assert!(remaining <= Duration::from_secs(5));
            phase("device wake and resume");
            host_input.write_all(b"A").unwrap();
            wait_for_wake(
                &handle,
                epoch,
                &["device_ram", "device_completion", "virtio_interrupt"],
            );
            assert_eq!(handle.is_paused(), Ok(true));
            handle.resume().unwrap();
            if let Some((idle_wall, idle_cpu_ns)) = idle_sample {
                eprintln!(
                    "NATIVE_IDLE_DIAG {} {} {}",
                    idle_wall.as_nanos(),
                    idle_cpu_ns,
                    arm_to_pause.as_nanos()
                );
                assert_eq!(output.next().unwrap().unwrap(), "DONE");
                phase("guest exit");
                return;
            }

            phase("timer wake and resume");
            assert_eq!(output.next().unwrap().unwrap(), "TIMER");
            let (epoch, remaining) = claim(&handle);
            assert!(remaining < Duration::from_millis(600));
            wait_for_wake(&handle, epoch, &["guest_timer"]);
            assert_eq!(handle.is_paused(), Ok(true));
            handle.resume().unwrap();
            assert_eq!(output.next().unwrap().unwrap(), "TIMER_OK");

            phase("resume and rearm");
            let (epoch, _) = claim(&handle);
            handle.retire_guest_idle(epoch);
            let next = handle
                .resume_and_arm_guest_idle(epoch, Duration::from_secs(1))
                .unwrap();
            assert_ne!(next, epoch);
            claim_epoch(&handle, next);
            handle.resume().unwrap();

            host_input.write_all(b"B").unwrap();
            assert_eq!(output.next().unwrap().unwrap(), "DONE");
            phase("guest exit");
        });
        Ok(())
    })
    .unwrap();
}

fn process_cpu_ns() -> u64 {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|entry| {
            let stat = fs::read_to_string(entry.unwrap().path().join("schedstat")).unwrap();
            stat.split_ascii_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .sum()
}

fn claim(handle: &VmmHandle) -> (u64, Duration) {
    let epoch = handle.arm_guest_idle(Duration::from_secs(1)).unwrap();
    (epoch, claim_epoch(handle, epoch))
}

fn claim_epoch(handle: &VmmHandle, epoch: u64) -> Duration {
    let mut status = handle.guest_idle_status();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert_eq!(status.epoch, epoch);
        assert_eq!(status.wake_reason, None, "{status:?}");
        if status.offered && handle.pause_guest_idle(epoch).unwrap() {
            return Duration::from_nanos(status.remaining_ns);
        }
        assert!(Instant::now() < deadline, "{status:?}");
        status = handle.wait_guest_idle(status.sequence, Duration::from_millis(100));
    }
}

fn wait_for_wake(handle: &VmmHandle, epoch: u64, reasons: &[&str]) {
    let mut status = handle.guest_idle_status();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert_eq!(status.epoch, epoch);
        if let Some(seen) = &status.wake_reason {
            assert!(reasons.contains(&seen.as_str()), "{status:?}");
            return;
        }
        assert!(Instant::now() < deadline, "{status:?}");
        status = handle.wait_guest_idle(status.sequence, Duration::from_millis(100));
    }
}

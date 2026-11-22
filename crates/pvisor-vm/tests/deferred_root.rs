#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "init-blob",
    not(any(feature = "tee", feature = "aws-nitro", feature = "efi"))
))]

#[path = "lifecycle/firmware.rs"]
mod firmware;

use pvisor_guest::api::GuestConfig;
use pvisor_vm::api::{
    DeferredRoot, OverlayConfig, PermissionSemantics, RuntimeSupport, VmBuilder, VmConfiguration,
    VmPlatform, VmRuntime,
};
use std::{
    fs,
    io::{self, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};

const CHILD: &str = "PVISOR_DEFERRED_ROOT_TEST_DIR";
const WAIT: Duration = Duration::from_secs(20);

struct Runner(Child);
impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_vm(directory: &Path) {
    use std::io::Read;
    VmPlatform::init_logging("init_or_kernel=debug,virtio=warn");
    let mut vm = VmBuilder::new(1, 128).unwrap();
    vm.set_firmware_path(firmware::external_firmware().unwrap())
        .unwrap();
    let directory = directory.to_owned();
    vm.deferred_root(move || {
        let mut host = UnixStream::connect(directory.join("gate"))?;
        host.set_read_timeout(Some(WAIT))?;
        let mut release = [0];
        host.read_exact(&mut release)?;
        if release != [1] {
            return Err(io::Error::other("request rejected"));
        }
        Ok(DeferredRoot {
            overlay: OverlayConfig {
                lower_dirs: vec![directory.join("image")],
                lower_mutability: vec![],
                upper_dir: directory.join("upper"),
                work_dir: Some(directory.join("work")),
                preimage_dir: None,
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            },
            guest: GuestConfig {
                argv: vec!["/workload".into()],
                cwd: "/".into(),
                ..GuestConfig::default()
            },
        })
    })
    .unwrap();
    vm.run(|_| Ok(())).unwrap();
}

#[test]
#[ignore = "requires KVM, Linux firmware, and the x86_64 Linux musl Rust target"]
fn pid1_waits_for_request_root_then_runs_in_same_vm() {
    check_deferred_root("pid1_waits_for_request_root_then_runs_in_same_vm", true);
}

#[test]
#[ignore = "requires KVM and Linux firmware"]
fn rejected_request_root_does_not_launch_a_workload() {
    check_deferred_root("rejected_request_root_does_not_launch_a_workload", false);
}

fn check_deferred_root(test: &str, attach: bool) {
    if let Some(directory) = std::env::var_os(CHILD) {
        run_vm(Path::new(&directory));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    if attach {
        build_workload(root);
    }
    let listener = UnixListener::bind(root.join("gate")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut runner = Runner(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env(CHILD, root)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    let mut gate = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("gate accept: {error}"),
        }
        assert_eq!(
            runner.0.try_wait().unwrap(),
            None,
            "VM exited before its root request"
        );
        assert!(Instant::now() < deadline, "PID 1 never requested its root");
        std::thread::sleep(Duration::from_millis(1));
    };
    let image = root.join("image");
    assert!(
        !image.exists(),
        "VM requested a root before any image existed"
    );
    if attach {
        for name in ["image", "upper", "work"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        fs::rename(root.join("workload"), image.join("workload")).unwrap();
        fs::write(image.join("request"), b"late root").unwrap();
    }
    gate.write_all(&[u8::from(attach)]).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = runner.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(if attach { 0 } else { 125 }));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "VM did not continue after root attach"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    if attach {
        assert_eq!(fs::read(root.join("upper/result")).unwrap(), b"continued");
        assert!(!image.join("result").exists());
    } else {
        assert!(!root.join("upper").exists());
    }
}

fn build_workload(root: &Path) {
    let fixture = root.join("workload.rs");
    fs::write(
        &fixture,
        r#"fn main() {
            assert_eq!(std::fs::read("/request").unwrap(), b"late root");
            assert!(!std::path::Path::new("/.pvisor-rootfs").exists());
            assert_eq!(std::fs::read("/proc/1/comm").unwrap(), b"init.pvisor\n");
            std::fs::write("/result", b"continued").unwrap();
        }"#,
    )
    .unwrap();
    assert!(Command::new("rustc")
        .args([
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "linker=rust-lld",
            "-o",
        ])
        .arg(root.join("workload"))
        .arg(fixture)
        .status()
        .unwrap()
        .success());
}

use pvisor_shim::api::{RUNTIME_TYPE, Shim, ShimRuntime};
use std::{
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn run(args: &[&str]) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_containerd-shim-pvisor-v2"))
        .args(args)
        .env_clear()
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "shim failed to exit: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        0,
        "rejected startup/introspection created runtime state"
    );
    output
}

#[test]
fn runtime_identity_and_stateless_entry_are_canonical() {
    assert_eq!(RUNTIME_TYPE, "io.containerd.pvisor.v2");
    assert_eq!(std::mem::size_of::<Shim>(), 0);
    let _ = Shim;
    let _: fn() -> anyhow::Result<()> = Shim::run;
}

#[test]
#[cfg(not(target_os = "linux"))]
fn unsupported_host_returns_a_typed_error_without_service_startup() {
    for _ in 0..2 {
        let error = Shim::run().unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Unsupported
        );
    }
    for args in [
        &[][..],
        &["-info"][..],
        &["--pvisor-shim-internal-init"][..],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("only supports Linux hosts"));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn introspection_uses_the_wire_contract_without_containerd_environment() {
    use containerd_shim_protos::{protobuf::Message, types::introspection::RuntimeInfo};
    let output = run(&["-info"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let info = RuntimeInfo::parse_from_bytes(&output.stdout).unwrap();
    assert_eq!(info.name, RUNTIME_TYPE);
    assert_eq!(
        info.version.as_ref().unwrap().version,
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
#[cfg(target_os = "linux")]
fn invalid_startup_returns_errors_without_creating_runtime_state() {
    assert!(
        Shim::run().is_err(),
        "libtest arguments cannot start a containerd service"
    );
    for args in [&[][..], &["--not-a-shim-option"][..]] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn private_child_reentry_precedes_containerd_argument_parsing() {
    let output = run(&["--pvisor-shim-internal-init"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing PVISOR_SHIM_INIT_PLAN"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

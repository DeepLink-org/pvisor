use std::process::Command;

#[test]
fn snapshot_is_a_discoverable_builtin_without_companions() {
    let binary = env!("CARGO_BIN_EXE_pvisor");
    let root = Command::new(binary)
        .arg("--help")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(root.status.success());
    assert!(String::from_utf8_lossy(&root.stdout).contains("  snapshot "));
    let help = Command::new(binary)
        .args(["snapshot", "--help"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for command in ["run", "save", "restore", "list", "delete", "gc"] {
        assert!(text.contains(&format!("  {command} ")), "{text}");
    }
    assert!(!text.contains("  runner "));
    assert!(!text.contains("  ram-watchdog "));
    assert!(!text.contains("  socket-watchdog "));
    assert!(text.contains("fork"), "{text}");
}

#[test]
fn invalid_resources_and_conflicting_launch_modes_fail_before_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("must-not-be-created");
    for arguments in [
        vec![
            "run", "--name", "a", "--rootfs", "/unused", "--cpus", "0", "--", "bash",
        ],
        vec![
            "run", "--name", "a", "--rootfs", "/unused", "--memory", "0", "--", "bash",
        ],
        vec!["run", "--name", "a", "--rootfs", "/unused"],
        vec![
            "run",
            "--name",
            "a",
            "--rootfs",
            "/unused",
            "--ram-storage",
            "invalid",
            "--",
            "bash",
        ],
        vec![
            "run",
            "--name",
            "a",
            "--rootfs",
            "/unused",
            "--native-init",
            "--",
            "bash",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["snapshot", "--store"])
            .arg(&store)
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!store.exists());
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn socket_watchdog_reaps_on_eof_and_preserves_replaced_regular_files() {
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        os::unix::{fs::DirBuilderExt, net::UnixListener},
        process::Stdio,
    };
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    let parent = std::path::PathBuf::from(format!("/tmp/pvisor-snapshots-{}", unsafe {
        libc::geteuid()
    }));
    match fs::DirBuilder::new().mode(0o700).create(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("{error}"),
    }
    let digest = Sha256::digest(directory.as_os_str().as_encoded_bytes());
    let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    let socket = parent.join(format!("{name}.sock"));
    for replaced in [false, true] {
        let listener = UnixListener::bind(&socket).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["snapshot", "socket-watchdog"])
            .arg(&directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if replaced {
            fs::remove_file(&socket).unwrap();
            fs::write(&socket, b"replacement must survive").unwrap();
        }
        drop(child.stdin.take());
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.success(), !replaced);
        if replaced {
            assert_eq!(fs::read(&socket).unwrap(), b"replacement must survive");
            fs::remove_file(&socket).unwrap();
        } else {
            assert!(!socket.exists());
        }
        drop(listener);
    }
}

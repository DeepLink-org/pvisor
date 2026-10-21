//! Docker-profile command routing and opt-in native VM acceptance.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command.env("PVISOR_DOCKER_HOME", home).arg("docker");
    command
}

#[test]
fn docker_help_and_empty_queries_are_inert_and_reject_unsupported_commands() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    for name in [
        "run", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm", "logs",
    ] {
        let output = command(&home).args([name, "--help"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
    }
    assert!(!home.exists(), "help created a catalog or owner");
    for args in [
        vec!["build", "."],
        vec!["run", "--privileged", "image"],
        vec!["compose", "up"],
    ] {
        assert!(!command(&home).args(args).output().unwrap().status.success());
    }
    assert!(!home.exists(), "invalid commands changed storage");
    let output = command(&home)
        .args(["run", "--privileged", "image"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    for args in [vec!["images", "-q"], vec!["ps", "-aq"]] {
        let output = command(&home).args(args).output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    assert_eq!(fs::read_dir(home.join("instances")).unwrap().count(), 0);
}

/// A real CLI process with a fake guest, so descriptor and adapter behavior is
/// exercised without a VM. In particular, Linux cannot reopen a socket via /dev/stdin.
#[test]
fn docker_exec_preserves_socket_stdin_options_output_and_status() {
    use fs2::FileExt;
    use pvisor::docker::{
        GuestReply, GuestRequest, GuestStdin, read_guest_frame, write_guest_frame,
    };
    use pvisor_container::api::ExecOutcome;
    use std::{
        io::Write,
        os::{
            fd::OwnedFd,
            unix::{
                fs::DirBuilderExt,
                net::{UnixListener, UnixStream},
            },
        },
        time::{Duration, Instant},
    };

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    pvisor::docker::Store::open(home.clone()).unwrap();
    let id = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let instance = home.join("instances").join(&id);
    fs::create_dir(&instance).unwrap();
    let owner = fs::File::create(instance.join("owner.lock")).unwrap();
    owner.lock_exclusive().unwrap();
    fs::write(instance.join("instance.json"), serde_json::to_vec(&serde_json::json!({
        "schema":1, "id":id, "name":"exec-fixture", "state":"running", "generation":generation,
        "started_ms":0, "exit_code":null, "created_ms":0,
        "argv":["sleep", "30"], "env":{"BASE":"inherited"}, "cwd":"/", "user":"",
        "image":{"reference":"fixture:latest", "id":"fixture", "digest":"fixture", "rootfs":"/",
            "env":{}, "entrypoint":[], "cmd":[], "cwd":"/", "user":"", "stop_signal":15, "volumes":[], "created_ms":0},
        "config":{"name":null,"image":"fixture:latest","command":[],"entrypoint":null,"env":{},
            "cwd":null,"user":null,"mounts":[],"ports":[],"cpus":1,"memory_mib":128,"auto_remove":false,
            "interactive":false,"tty":null,"network":false}
    })).unwrap()).unwrap();
    let socket_dir = PathBuf::from(format!("/tmp/pvisor-docker-{}", unsafe { libc::geteuid() }));
    match fs::DirBuilder::new().mode(0o700).create(&socket_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("socket directory: {error}"),
    }
    let socket_path = socket_dir.join(format!("{}-{}.sock", &id[..12], &generation[..8]));
    struct SocketPath(PathBuf);
    impl Drop for SocketPath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _socket_path = SocketPath(socket_path.clone());
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("CLI did not connect: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let GuestRequest::Exec(request) = read_guest_frame(&mut stream).unwrap() else {
            panic!("expected exec");
        };
        assert_eq!(request.argv, ["cat", "literal argument"]);
        assert_eq!(request.env["VALUE"], "with spaces");
        assert!(!request.replace_env);
        assert!(request.limits.is_none());
        assert!(request.output_timeout_ms.is_none());
        assert!(request.tty.is_none());
        assert_eq!(request.user.as_deref(), Some("123:456"));
        assert_eq!(request.cwd, Some(PathBuf::from("/work")));
        write_guest_frame(&mut stream, &GuestReply::Started { pid: 42 }).unwrap();
        let mut input = Vec::new();
        while let GuestStdin::Data { bytes } = read_guest_frame(&mut stream)
            .unwrap_or_else(|error| panic!("stdin failed after {} bytes: {error}", input.len()))
        {
            input.extend(bytes);
        }
        assert_eq!(input, b"socket-input\0\xff");
        write_guest_frame(
            &mut stream,
            &GuestReply::Output {
                stream: 1,
                bytes: input,
            },
        )
        .unwrap();
        write_guest_frame(
            &mut stream,
            &GuestReply::Output {
                stream: 2,
                bytes: b"diagnostic".to_vec(),
            },
        )
        .unwrap();
        write_guest_frame(
            &mut stream,
            &GuestReply::ExecExit(ExecOutcome {
                code: 23,
                signal: None,
                output_complete: true,
            }),
        )
        .unwrap();
    });
    let (stdin, mut input) = UnixStream::pair().unwrap();
    input.write_all(b"socket-input\0\xff").unwrap();
    input.shutdown(std::net::Shutdown::Write).unwrap();
    let output = command(&home)
        .args([
            "exec",
            "-i",
            "-e",
            "VALUE=with spaces",
            "-w",
            "/work",
            "-u",
            "123:456",
            "exec-fixture",
            "cat",
            "literal argument",
        ])
        .stdin(Stdio::from(OwnedFd::from(stdin)))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    worker.join().unwrap();
    assert_eq!(output.stdout, b"socket-input\0\xff");
    assert_eq!(output.stderr, b"diagnostic");
}

struct Native {
    _temporary: tempfile::TempDir,
    home: PathBuf,
    binary: PathBuf,
}
impl Native {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("pvisor");
        fs::copy(env!("CARGO_BIN_EXE_pvisor"), &binary).unwrap();
        #[cfg(target_os = "macos")]
        {
            let firmware = Path::new(env!("CARGO_BIN_EXE_pvisor"))
                .parent()
                .unwrap()
                .join("libkrunfw.5.dylib")
                .canonicalize()
                .expect("build the packaged macOS firmware first");
            std::os::unix::fs::symlink(firmware, temporary.path().join("libkrunfw.5.dylib"))
                .unwrap();
            assert!(
                Command::new("codesign")
                    .args(["--force", "--sign", "-", "--entitlements"])
                    .arg(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../pvisor/macos-hypervisor.entitlements")
                    )
                    .arg(&binary)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let home = temporary.path().join("store");
        pvisor::docker::Store::open(home.clone()).unwrap();
        let rootfs = PathBuf::from(
            std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS")
                .expect("set PVISOR_DOCKER_TEST_ROOTFS to a native Linux rootfs with sh/cat/sleep"),
        );
        let image = pvisor::docker::Image {
            reference: "fixture:latest".into(),
            id: format!("sha256:{}", "1".repeat(64)),
            digest: format!("sha256:{}", "2".repeat(64)),
            rootfs: rootfs.canonicalize().unwrap(),
            env: BTreeMap::new(),
            entrypoint: Vec::new(),
            cmd: Vec::new(),
            cwd: "/".into(),
            user: String::new(),
            stop_signal: 15,
            volumes: Vec::new(),
            created_ms: 0,
        };
        // This is an isolated catalog fixture, never a real user's image store.
        fs::write(
            home.join("images.json"),
            serde_json::to_vec(&[image]).unwrap(),
        )
        .unwrap();
        Self {
            _temporary: temporary,
            home,
            binary,
        }
    }
    fn call(&self, args: &[&str], stdin: Option<&[u8]>, expected: i32) -> Output {
        let mut child = Command::new(&self.binary)
            .env("PVISOR_DOCKER_HOME", &self.home)
            .arg("docker")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(stdin) = stdin {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(stdin).unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn job(&self, script: &str, expected: i32) {
        let root = self._temporary.path();
        let mut command = Command::new(&self.binary);
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            fs::create_dir(root.join(directory)).unwrap();
            command.env(key, root.join(directory));
        }
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let output = command
            .current_dir(workspace)
            .env("PVISOR_RUN_HOME", root.join("jobs"))
            .env("PVISOR_STARTUP_TIMING", "0")
            .args(["run", "--executor", "vm", "--rootfs"])
            .arg(std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS").unwrap())
            .args(["--stdio", "capture", "--", "/bin/sh", "-c", script])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .env("PVISOR_DOCKER_HOME", &self.home)
            .args(["docker", "rm", "-f", "native-test"])
            .output();
    }
}

#[test]
#[ignore = "requires native virtualization, local sockets and PVISOR_DOCKER_TEST_ROOTFS"]
fn job_native_vm_uses_shared_supervision() {
    Native::new().job(
        include_str!("../../pvisor-guest/tests/fixtures/reap_orphans.sh"),
        0,
    );
    Native::new().job("echo job-stderr >&2; exit 7", 7);
}

#[test]
#[ignore = "requires native virtualization, local sockets and PVISOR_DOCKER_TEST_ROOTFS"]
fn docker_native_vm_lifecycle_streams_terminal_and_persistent_upper() {
    let native = Native::new();
    let reap_orphans = include_str!("../../pvisor-guest/tests/fixtures/reap_orphans.sh");
    native.call(
        &[
            "run",
            "--rm",
            "--network",
            "none",
            "fixture",
            "/bin/sh",
            "-c",
            reap_orphans,
        ],
        None,
        0,
    );
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mapping = format!("127.0.0.1:{}:8080", occupied.local_addr().unwrap().port());
    let output = native.call(
        &[
            "run",
            "-d",
            "--rm",
            "-p",
            &mapping,
            "fixture",
            "/bin/sleep",
            "60",
        ],
        None,
        125,
    );
    assert!(
        output.stdout.is_empty(),
        "failed startup printed a container ID"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to start"));
    drop(occupied);
    native.call(
        &[
            "run",
            "--rm",
            "--network",
            "none",
            "fixture",
            "/no-such-command",
        ],
        None,
        127,
    );
    let output = native.call(
        &[
            "run",
            "--rm",
            "--network",
            "none",
            "fixture",
            "/bin/sh",
            "-c",
            "echo out; echo err >&2; exit 7",
        ],
        None,
        7,
    );
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
    let output = native.call(
        &[
            "run",
            "-d",
            "--name",
            "native-test",
            "--network",
            "none",
            "fixture",
            "/bin/sh",
            "-c",
            "echo retained >/retained; trap 'exit 0' TERM; while :; do sleep 1; done",
        ],
        None,
        0,
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim().len(), 64);
    native.call(&["exec", "native-test", "/no-such-command"], None, 127);
    native.call(
        &["exec", "native-test", "/bin/sh", "-c", reap_orphans],
        None,
        0,
    );
    std::thread::scope(|scope| {
        for code in [7, 23, 0, 19] {
            let native = &native;
            scope.spawn(move || {
                native.call(
                    &[
                        "exec",
                        "native-test",
                        "/bin/sh",
                        "-c",
                        &format!("exit {code}"),
                    ],
                    None,
                    code,
                );
            });
        }
    });
    let output = native.call(
        &["exec", "-i", "native-test", "/bin/cat"],
        Some(b"input\n"),
        0,
    );
    assert_eq!(output.stdout, b"input\n");
    let output = native.call(
        &[
            "exec",
            "-t",
            "native-test",
            "/bin/sh",
            "-c",
            "test -t 0 && test -t 1 && echo tty",
        ],
        None,
        0,
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("tty"));
    native.call(&["stop", "-t", "2", "native-test"], None, 0);
    native.call(&["start", "native-test"], None, 0);
    let output = native.call(&["exec", "native-test", "/bin/cat", "/retained"], None, 0);
    assert_eq!(output.stdout, b"retained\n");
    native.call(&["rm", "-f", "native-test"], None, 0);

    let mut child = Command::new(&native.binary)
        .env("PVISOR_DOCKER_HOME", &native.home)
        .args([
            "docker",
            "run",
            "--rm",
            "-i",
            "--name",
            "native-test",
            "--network",
            "none",
            "fixture",
            "/bin/sh",
            "-c",
            "read ready; trap 'exit 0' TERM; echo ready; while :; do sleep 1; done",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::{Read, Write};
    child.stdin.as_mut().unwrap().write_all(b"ready\n").unwrap();
    let mut ready = [0; 6];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready\n");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

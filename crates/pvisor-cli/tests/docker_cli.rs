//! Docker-profile command routing and opt-in native VM acceptance.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command.env("PVISOR_DOCKER_HOME", home);
    command
}

#[test]
fn docker_help_and_empty_queries_are_inert_and_reject_unsupported_commands() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    for name in [
        "run", "build", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm", "logs",
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
        vec!["build", "--privileged", "."],
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

#[test]
fn docker_run_rejects_a_foreign_platform_before_pulling() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    let foreign = if cfg!(target_arch = "aarch64") {
        "linux/amd64"
    } else {
        "linux/arm64"
    };
    let output = command(&home)
        .args(["run", "--platform", foreign, "unused-image", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cross-architecture emulation"), "{stderr}");
    assert!(!stderr.contains("Pulling"), "{stderr}");
    assert_eq!(fs::read_dir(home.join("instances")).unwrap().count(), 0);
}

#[test]
fn pull_and_implicit_run_report_waiting_before_network_or_vm_start() {
    use fs2::FileExt;
    use sha2::{Digest, Sha256};
    use std::{
        io::{BufRead, BufReader},
        sync::mpsc,
        time::Duration,
    };
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("store");
    pvisor::docker::Store::open(home.clone()).unwrap();
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => panic!("unsupported test architecture: {other}"),
    };
    // Hold the isolated OCI fixture's preparation lock. Neither command can
    // reach a registry; progress must be flushed while the request is waiting.
    let key = serde_json::to_vec(&(
        "registry-1.docker.io",
        "library/ubuntu",
        "latest",
        architecture,
    ))
    .unwrap();
    let locks = home.join("images/locks");
    fs::create_dir_all(&locks).unwrap();
    let hash: String = Sha256::digest(key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let lock = fs::File::create(locks.join(format!("prepare-{hash}.lock"))).unwrap();
    lock.lock_exclusive().unwrap();
    for args in [
        vec!["pull", "ubuntu:latest"],
        vec!["run", "ubuntu:latest", "echo", "hello"],
    ] {
        let mut child = command(&home)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let lines: Vec<_> = BufReader::new(stderr)
                .lines()
                .take(2)
                .map(Result::unwrap)
                .collect();
            let _ = send.send(lines);
        });
        let lines = receive.recv_timeout(Duration::from_secs(5));
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        reader.join().unwrap();
        assert_eq!(
            lines.unwrap(),
            ["Pulling ubuntu:latest", "Waiting for image preparation..."]
        );
        assert!(output.stdout.is_empty());
    }
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
            platform: if cfg!(target_arch = "aarch64") {
                "linux/arm64"
            } else {
                "linux/amd64"
            }
            .into(),
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
            .args(["spawn", "--executor", "vm", "--rootfs"])
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
            .args(["rm", "-f", "native-test"])
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

#[test]
fn docker_build_cli_exports_scratch_image_and_keeps_tags_on_error() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("store");
    let context = temporary.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), "hello").unwrap();
    fs::write(context.join("Dockerfile"), "FROM scratch AS source\nARG MESSAGE=default\nENV MESSAGE=$MESSAGE LITERAL='${MESSAGE}'\nCOPY payload /payload\nFROM source AS final\nARG MESSAGE\nENV INHERITED=$MESSAGE\nCMD [\"/payload\"]\n").unwrap();
    let iid = temporary.path().join("iid");
    let archive = temporary.path().join("image.tar");
    let output = command(&home)
        .args([
            "build",
            "--no-cache",
            "-q",
            "-t",
            "built:test",
            "--build-arg",
            "MESSAGE=custom",
            "--iidfile",
        ])
        .arg(&iid)
        .arg("--output")
        .arg(format!("type=oci,dest={}", archive.display()))
        .arg(&context)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(&iid).unwrap(), output.stdout);
    let images = pvisor::docker::Store::open(home.clone())
        .unwrap()
        .images()
        .unwrap();
    let image = images.iter().find(|i| i.reference == "built:test").unwrap();
    assert_eq!(image.env["MESSAGE"], "custom");
    assert_eq!(image.env["INHERITED"], "custom");
    assert_eq!(image.env["LITERAL"], "${MESSAGE}");
    assert_eq!(fs::read(image.rootfs.join("payload")).unwrap(), b"hello");
    assert!(archive.is_file());
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY absent /payload\n",
    )
    .unwrap();
    assert!(
        !command(&home)
            .args(["build", "-q", "-t", "built:test"])
            .arg(&context)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        pvisor::docker::Store::open(home).unwrap().images().unwrap()[0].id,
        image.id
    );
}

#[test]
#[ignore = "requires native virtualization, local sockets and PVISOR_DOCKER_TEST_ROOTFS"]
fn docker_build_native_vm_multistep_roundtrip_failure_and_timeout() {
    let native = Native::new();
    let context = native._temporary.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), "copied").unwrap();
    let dockerfile = context.join("Dockerfile");
    fs::write(&dockerfile, "FROM fixture AS compiler\nARG MESSAGE=hello\nCOPY payload /input\nRUN printf '%s' \"$MESSAGE\" > /result; cat /input >> /result; cp /result /removed; ln /result /alias; chmod 0750 /result; chown 123:456 /result\nRUN test -f /result && rm /removed\nRUN (sleep 2; echo leaked > /late) >/dev/null 2>&1 & echo ready > /ready\nRUN sleep 3; test ! -e /late\nFROM fixture\nCOPY --from=compiler /result /output\nCMD [\"/bin/cat\",\"/output\"]\n").unwrap();
    let context = context.to_str().unwrap();
    native.call(
        &[
            "build",
            "--network=none",
            "--timeout=30",
            "-t",
            "built:native",
            context,
        ],
        None,
        0,
    );
    let output = native.call(&["run", "--rm", "--network=none", "built:native"], None, 0);
    assert_eq!(output.stdout, b"hellocopied");
    let old = pvisor::docker::Store::open(native.home.clone())
        .unwrap()
        .images()
        .unwrap()
        .into_iter()
        .find(|i| i.reference == "built:native")
        .unwrap()
        .id;
    fs::write(
        &dockerfile,
        "FROM fixture\nRUN echo failed > /not-published; exit 7\n",
    )
    .unwrap();
    let failed = native.call(
        &["build", "--network=none", "-t", "built:native", context],
        None,
        1,
    );
    assert!(String::from_utf8_lossy(&failed.stderr).contains("status 7"));
    fs::write(&dockerfile, "FROM fixture\nRUN sleep 30\n").unwrap();
    let start = std::time::Instant::now();
    let failed = native.call(
        &[
            "build",
            "--network=none",
            "--timeout=2",
            "-t",
            "built:native",
            context,
        ],
        None,
        1,
    );
    assert!(String::from_utf8_lossy(&failed.stderr).contains("time budget"));
    assert!(start.elapsed() < std::time::Duration::from_secs(15));
    let images = pvisor::docker::Store::open(native.home.clone())
        .unwrap()
        .images()
        .unwrap();
    assert_eq!(
        images
            .iter()
            .find(|i| i.reference == "built:native")
            .unwrap()
            .id,
        old
    );
    assert_eq!(fs::read_dir(native.home.join("builds")).unwrap().count(), 0);
    assert_eq!(
        fs::read_dir(native.home.join("instances")).unwrap().count(),
        0
    );
}

#[test]
#[ignore = "requires native virtualization, process inspection and PVISOR_DOCKER_TEST_ROOTFS"]
fn docker_build_frontend_death_terminates_its_vm() {
    let native = Native::new();
    let context = native._temporary.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM fixture\nRUN echo running > /marker; sleep 60\n",
    )
    .unwrap();
    let mut frontend = Command::new(&native.binary)
        .env("PVISOR_DOCKER_HOME", &native.home)
        .args(["build", "--network=none", "-t", "never:published"])
        .arg(&context)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Kill only this test's frontend/VM, identified by both parent PID and private store.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let vm = loop {
        let listing = Command::new("ps")
            .args(["-axo", "pid=,ppid=,command="])
            .output()
            .unwrap();
        let pid = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: i32 = fields.next()?.parse().ok()?;
                let parent: u32 = fields.next()?.parse().ok()?;
                (parent == frontend.id()
                    && line.contains("--pvisor-internal-docker-vm")
                    && line.contains(native.home.to_str().unwrap()))
                .then_some(pid)
            });
        if let Some(pid) = pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "build VM did not start"
        );
        assert!(
            frontend.try_wait().unwrap().is_none(),
            "build ended prematurely"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // Allow the self-exec VM to install its lifetime reader, then simulate SIGKILL.
    std::thread::sleep(std::time::Duration::from_millis(500));
    frontend.kill().unwrap();
    frontend.wait().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = Command::new("ps")
            .args(["-o", "stat=", "-p", &vm.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&status.stdout);
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "VMM survived frontend death: {vm} {state}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        pvisor::docker::Store::open(native.home.clone())
            .unwrap()
            .images()
            .unwrap()
            .iter()
            .all(|i| i.reference != "never:published")
    );
}

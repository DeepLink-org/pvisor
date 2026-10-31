//! Offline, real Linux/KVM acceptance for explicit Docker staged binds.
//! Run with PVISOR_DOCKER_TEST_ROOTFS and (for dynamic builds)
//! PVISOR_TEST_LIBRARY_DIR, filtering `test(native_staged_)` so the reused
//! docker_cli module's independent acceptance cases are not selected.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[path = "docker_cli.rs"]
mod docker_fixture;

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    native: docker_fixture::Native,
    sources: Vec<PathBuf>,
}

impl Fixture {
    fn new(count: usize) -> Self {
        assert!(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/kvm")
                .is_ok()
        );
        assert!(
            std::env::var_os("PVISOR_DOCKER_TEST_IMAGE").is_none(),
            "these tests are offline: unset PVISOR_DOCKER_TEST_IMAGE"
        );
        assert!(
            std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS").is_some(),
            "set PVISOR_DOCKER_TEST_ROOTFS to a local Linux rootfs with sh/cat/sleep"
        );
        let native = docker_fixture::Native::new();
        let sources = (0..count)
            .map(|index| {
                let source = native._temporary.path().join(format!("source-{index}"));
                fs::create_dir(&source).unwrap();
                fs::write(source.join("edit"), "original\n").unwrap();
                fs::write(source.join("delete"), "delete me\n").unwrap();
                source
            })
            .collect();
        Self { native, sources }
    }

    fn call(&self, args: &[&str]) -> Output {
        let root = self.native._temporary.path();
        let mut child = Command::new(&self.native.binary)
            .current_dir(root)
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("PVISOR_RUN_HOME", root.join("jobs"))
            .env("PVISOR_DOCKER_HOME", &self.native.home)
            .env("PVISOR_IMAGE_STORE", self.native.home.join("images"))
            .env_remove("PVISOR_STARTUP_TIMING")
            .env_remove("PVISOR_STARTUP_PROFILE")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Drain both pipes while polling; a VM/owner regression must not hang nextest.
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        thread::scope(|scope| {
            let read = |mut pipe: Box<dyn std::io::Read + Send>| {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            };
            let out = scope.spawn(move || read(Box::new(stdout)));
            let err = scope.spawn(move || read(Box::new(stderr)));
            let deadline = Instant::now() + Duration::from_secs(60);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let status = child.wait().unwrap();
                    panic!("CLI timed out: {args:?}, status={status}");
                }
                thread::sleep(Duration::from_millis(20));
            };
            Output {
                status,
                stdout: out.join().unwrap(),
                stderr: err.join().unwrap(),
            }
        })
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.call(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        output
    }

    fn refused(&self, args: &[&str], diagnostic: &str) -> Output {
        let output = self.call(args);
        assert!(
            !output.status.success(),
            "unexpected success: {args:?}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{args:?}: expected {diagnostic:?}: {output:?}"
        );
        output
    }

    fn run(&self, script: &str, detached: bool) {
        let volumes: Vec<_> = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| format!("{}:/workspace-{index}:stage", source.display()))
            .collect();
        let mut args = vec![
            "run",
            "--name",
            "native-test",
            "--network",
            "none",
            "--cpus",
            "1",
            "-m",
            "128m",
        ];
        if detached {
            args.push("-d");
        }
        for volume in &volumes {
            args.extend(["-v", volume]);
        }
        args.extend(["fixture", "/bin/sh", "-c", script]);
        self.ok(&args);
    }

    fn instance(&self) -> Value {
        let store = pvisor::docker::Store::open(self.native.home.clone()).unwrap();
        serde_json::to_value(store.resolve("native-test").unwrap()).unwrap()
    }

    fn stage(&self, index: usize) -> PathBuf {
        let store = pvisor::docker::Store::open(self.native.home.clone()).unwrap();
        store.staged_mounts("native-test").unwrap()[index]
            .stage
            .clone()
            .unwrap()
    }

    fn wait_exited(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self.instance()["state"] == "exited" {
                return;
            }
            assert!(Instant::now() < deadline, "container did not exit");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn json(output: Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
}

// Exclude atime, which our own observation can change; include object identity,
// permissions, mtimes/ctimes and bytes to catch evidence rewrites or new files.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u64>, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, (Vec<u64>, Vec<u8>)>) {
        let m = fs::symlink_metadata(path).unwrap();
        let bytes = if m.is_file() {
            fs::read(path).unwrap()
        } else if m.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            vec![]
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            (
                vec![
                    m.dev(),
                    m.ino(),
                    m.mode() as u64,
                    m.uid() as u64,
                    m.gid() as u64,
                    m.len(),
                    m.mtime() as u64,
                    m.mtime_nsec() as u64,
                    m.ctime() as u64,
                    m.ctime_nsec() as u64,
                ],
                bytes,
            ),
        );
        if m.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_partial_apply_drop_and_restart_generation() {
    let f = Fixture::new(1);
    let before = snapshot(&f.sources[0]);
    f.run("echo changed >/workspace-0/edit; echo added >/workspace-0/added; rm /workspace-0/delete; echo rootfs-only >/rootfs-only", false);
    assert_eq!(
        snapshot(&f.sources[0]),
        before,
        "guest writes escaped staging"
    );
    let instance = f.instance();
    let stage = f.stage(0);
    let review = json(f.ok(&["review", "native-test", "--json"]));
    assert_eq!(
        review["filesystem"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["added", "delete", "edit"],
        "rootfs writes must not become host candidates: {review}"
    );

    f.refused(&["start", "native-test"], "unaccepted staged changes");
    assert_eq!(f.instance()["generation"], instance["generation"]);
    f.ok(&["apply", "native-test", "--path", "edit", "--json"]);
    assert_eq!(fs::read(f.sources[0].join("edit")).unwrap(), b"changed\n");
    assert!(!f.sources[0].join("added").exists());
    assert!(f.sources[0].join("delete").exists());
    f.refused(&["start", "native-test"], "unaccepted staged changes");
    f.ok(&["drop", "native-test"]);
    assert_eq!(fs::read(f.sources[0].join("edit")).unwrap(), b"changed\n");
    assert!(!f.sources[0].join("added").exists());
    assert!(f.sources[0].join("delete").exists());
    f.ok(&["start", "native-test"]);
    f.wait_exited();
    assert_ne!(f.instance()["generation"], instance["generation"]);
    assert_ne!(f.stage(0), stage);
    // The old generation cannot acquire current-generation writeback authority.
    f.refused(
        &["apply", stage.to_str().unwrap(), "--all", "--json"],
        "stale container generation",
    );
    f.ok(&["drop", "native-test"]);
}

fn preview_is_readonly(format: &str) {
    let f = Fixture::new(1);
    f.run(
        "echo guest >/workspace-0/edit; echo rootfs-only >/rootfs-only",
        false,
    );
    let stage = f.stage(0);
    let evidence = snapshot(&stage);
    let host = snapshot(&f.sources[0]);
    let instance = f.instance();
    let output = f.ok(&["apply", "native-test", "--all", "--dry-run", format]);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("rootfs-only"));
    assert_eq!(snapshot(&f.sources[0]), host);
    assert_eq!(f.instance(), instance);
    let after = snapshot(&stage);
    let changed: Vec<_> = evidence
        .keys()
        .chain(after.keys())
        .filter(|path| evidence.get(*path) != after.get(*path))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|path| (path, evidence.get(path), after.get(path)))
        .collect();
    assert!(
        changed.is_empty(),
        "{format} preview changed stage/evidence: {changed:?}"
    );
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_json_preview_is_readonly() {
    preview_is_readonly("--json");
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_diff_preview_is_readonly() {
    preview_is_readonly("--diff");
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_multiple_mounts_require_exact_selection() {
    let f = Fixture::new(2);
    f.run(
        "echo left >/workspace-0/result; echo right >/workspace-1/result",
        false,
    );
    let before: Vec<_> = f.sources.iter().map(|path| snapshot(path)).collect();
    for operation in ["review", "apply", "drop"] {
        let mut args = vec![operation, "native-test"];
        if operation == "apply" {
            args.extend(["--all", "--json"]);
        }
        f.refused(&args, "exactly one staged bind");
        for invalid in [
            "/workspace",
            "/workspace-1/child",
            "workspace-1",
            "/missing",
        ] {
            let mut selected = args.clone();
            selected.extend(["--mount", invalid]);
            f.refused(&selected, "exactly name");
        }
    }
    for (path, expected) in f.sources.iter().zip(before) {
        assert_eq!(snapshot(path), expected);
    }
    f.ok(&["review", "native-test", "--mount", "/workspace-1", "--json"]);
    let left = snapshot(&f.stage(0));
    f.ok(&[
        "apply",
        "native-test",
        "--mount",
        "/workspace-1",
        "--path",
        "result",
        "--json",
    ]);
    assert_eq!(snapshot(&f.stage(0)), left);
    assert!(!f.sources[0].join("result").exists());
    assert_eq!(fs::read(f.sources[1].join("result")).unwrap(), b"right\n");
    f.ok(&["drop", "native-test", "--mount", "/workspace-0"]);
    assert!(!f.sources[0].join("result").exists());
    assert_eq!(fs::read(f.sources[1].join("result")).unwrap(), b"right\n");
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_live_operations_are_refused() {
    let f = Fixture::new(1);
    f.run("echo ready >/workspace-0/result; exec sleep 60", true);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = f.ok(&[
            "exec",
            "native-test",
            "/bin/sh",
            "-c",
            "if test -f /workspace-0/result; then cat /workspace-0/result; fi",
        ]);
        if output.stdout == b"ready\n" {
            break;
        }
        assert!(Instant::now() < deadline, "guest never wrote staged file");
        thread::sleep(Duration::from_millis(50));
    }
    for selector in [
        "native-test".to_owned(),
        f.stage(0).to_str().unwrap().to_owned(),
    ] {
        for operation in ["review", "apply", "drop"] {
            let mut args = vec![operation, &selector];
            if operation == "apply" {
                args.extend(["--all", "--json"]);
            }
            let output = f.call(&args);
            assert!(
                !output.status.success(),
                "live operation succeeded: {args:?}: {output:?}"
            );
        }
    }
    assert!(!f.sources[0].join("result").exists());
    f.ok(&["stop", "-t", "1", "native-test"]);
    f.ok(&["review", "native-test", "--json"]);
    f.ok(&["drop", "native-test"]);
    assert!(!f.sources[0].join("result").exists());
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_source_content_conflict_is_rejected() {
    let f = Fixture::new(1);
    f.run("echo guest >/workspace-0/edit", false);
    fs::write(f.sources[0].join("edit"), "concurrent host edit\n").unwrap();
    let before = snapshot(&f.sources[0]);
    let output = f.call(&["apply", "native-test", "--all", "--json"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let response = json(output);
    assert_eq!(response["status"], "blocked");
    assert!(
        response["blockers"]
            .to_string()
            .contains("target changed after staging"),
        "{response}"
    );
    assert_eq!(snapshot(&f.sources[0]), before);
    f.ok(&["drop", "native-test"]);
    assert_eq!(snapshot(&f.sources[0]), before);
}

#[test]
#[ignore = "real Linux KVM; local PVISOR_DOCKER_TEST_ROOTFS and firmware required"]
fn native_staged_replaced_source_directory_identity_is_rejected() {
    let f = Fixture::new(1);
    f.run("echo guest >/workspace-0/edit", false);
    let moved = f.native._temporary.path().join("original-source");
    fs::rename(&f.sources[0], &moved).unwrap();
    fs::create_dir(&f.sources[0]).unwrap();
    fs::write(f.sources[0].join("edit"), "original\n").unwrap();
    let before = snapshot(&f.sources[0]);
    let old = snapshot(&moved);
    let output = f.call(&["apply", "native-test", "--all", "--json"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let response = json(output);
    assert!(
        response.to_string().to_lowercase().contains("identity"),
        "{response}"
    );
    assert_eq!(snapshot(&f.sources[0]), before);
    assert_eq!(snapshot(&moved), old);
}

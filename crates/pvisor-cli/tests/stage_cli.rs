//! External stage CLI contracts; native coverage is opt-in and never applies rootfs changes.
#![cfg(unix)]

use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    binary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("pvstage-")
            .tempdir()
            .unwrap();
        for directory in [
            "home",
            "config",
            "cache",
            "data",
            "state",
            "runtime",
            "workspace",
        ] {
            fs::create_dir(root.path().join(directory)).unwrap();
        }
        Self {
            root,
            binary: PathBuf::from(env!("CARGO_BIN_EXE_pvisor")),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/sh")
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("PVISOR_RUN_HOME", self.path("jobs"))
            .env("PVISOR_IMAGE_STORE", self.path("images"))
            .env("PVISOR_DOCKER_HOME", self.path("docker"))
            .current_dir(self.path("workspace"))
            .stdin(Stdio::null());
        command
    }

    fn run(&self, command: &mut Command) -> Output {
        // Files avoid pipe-buffer deadlock and inherited-pipe waits after a timeout.
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        command
            .process_group(0)
            .stdout(stdout.try_clone().unwrap())
            .stderr(stderr.try_clone().unwrap());
        let mut child = command.spawn().expect("spawn CLI");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                // The test owns this process group, never the caller's group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                child.kill().ok();
                child.wait().unwrap();
                panic!(
                    "CLI exceeded 10s: {command:?}\nstdout: {}\nstderr: {}",
                    read_output(stdout),
                    read_output(stderr)
                );
            }
            thread::sleep(Duration::from_millis(10));
        };
        let output = Output {
            status,
            stdout: read_output(stdout).into_bytes(),
            stderr: read_output(stderr).into_bytes(),
        };
        if std::env::var_os("PVISOR_TEST_KEEP_ARTIFACTS").is_some() {
            fs::write(
                self.path(&format!("cli-{}.log", uuid::Uuid::new_v4())),
                format!(
                    "{command:?}\nstatus: {}\nstdout:\n{}\nstderr:\n{}\n",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                ),
            )
            .unwrap();
        }
        output
    }

    fn assert_inert(&self) {
        for name in ["jobs", "stage", "images", "docker"] {
            assert!(!self.path(name).exists(), "unexpected side effect: {name}");
        }
        for name in [
            "home",
            "config",
            "cache",
            "data",
            "state",
            "runtime",
            "workspace",
        ] {
            assert_eq!(
                fs::read_dir(self.path(name)).unwrap().count(),
                0,
                "changed {name}"
            );
        }
    }
}

// Opt in when debugging a failed native launch; retained roots can include host
// environment projections and must not be published without review.
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::env::var_os("PVISOR_TEST_KEEP_ARTIFACTS").is_some() {
            self.root.disable_cleanup(true);
            eprintln!(
                "stage CLI artifacts retained: {}",
                self.root.path().display()
            );
        }
    }
}

fn read_output(mut file: fs::File) -> String {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut output = String::new();
    file.read_to_string(&mut output).unwrap();
    output
}

fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn help_groups_stage_with_jobs_and_is_inert() {
    let fixture = Fixture::new();
    let output = fixture.run(fixture.command().arg("--help"));
    success(&output);
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    let (containers, agents) = help.split_once("Agent Commands:").expect("Job help group");
    assert!(
        !containers
            .lines()
            .any(|line| line.split_whitespace().next() == Some("stage"))
    );
    let agents = agents.split("Tools:").next().unwrap();
    let names: Vec<_> = agents
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    let spawn = names.iter().position(|name| *name == "spawn").unwrap();
    assert_eq!(names[spawn + 1], "stage");
    for args in [vec!["stage", "--help"], vec!["help", "stage"]] {
        let output = fixture.run(fixture.command().args(args));
        success(&output);
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        for text in [
            "COMMAND",
            "--env",
            "--workdir",
            "--memory",
            "--cpus",
            "--network",
            "--stage",
            "Linux",
            "drop",
        ] {
            assert!(help.contains(text), "missing {text}: {help}");
        }
        assert!(!help.contains("<IMAGE>"), "stage must not require an image");
    }
    fixture.assert_inert();
}

#[test]
fn invalid_options_and_resource_boundaries_have_no_side_effects() {
    let fixture = Fixture::new();
    for args in [
        vec!["--cpus", "0"],
        vec!["--cpus", "9"],
        vec!["--cpus", "1.5"],
        vec!["-m", "15m"],
        vec!["-m", "bogus"],
        vec!["--network", "host"],
        vec!["--network", "custom"],
        vec!["-v", "/host:/guest"],
        vec!["--volume", "/host:/guest"],
        vec!["-d"],
        vec!["--detach"],
        vec!["--rm"],
        vec!["--rootfs", "host"],
        vec!["--cpus"],
        vec!["--stage"],
        vec!["-e"],
        vec!["-w"],
    ] {
        let output = fixture.run(
            fixture
                .command()
                .arg("stage")
                .arg("--stage")
                .arg(fixture.path("stage"))
                .args(&args),
        );
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
        fixture.assert_inert();
    }
    for (cpus, memory, network) in [
        ("1", "16m", "none"),
        ("8", "1g", "bridge"),
        ("2", "536870912", "none"),
    ] {
        let output = fixture.run(fixture.command().args([
            "stage",
            "--cpus",
            cpus,
            "-m",
            memory,
            "--network",
            network,
            "--help",
        ]));
        success(&output);
        fixture.assert_inert();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn validation_and_literal_workload_options_do_not_start_a_job() {
    let fixture = Fixture::new();
    for (args, error) in [
        (vec!["-w", "relative"], "--workdir must be an absolute path"),
        (vec!["-e", "=bad"], "invalid environment name"),
        (
            vec!["--env-file", "missing.env"],
            "No such file or directory",
        ),
        (vec!["-it"], "requires an interactive terminal"),
        // After COMMAND, even --help and invalid stage flags belong to the guest.
        (
            vec![
                "-w",
                "relative",
                "/bin/echo",
                "--help",
                "--cpus",
                "99",
                "--rm",
            ],
            "--workdir must be an absolute path",
        ),
        (
            vec!["-w", "relative", "--", "/bin/echo", "--network", "host"],
            "--workdir must be an absolute path",
        ),
    ] {
        let output = fixture.run(
            fixture
                .command()
                .args(["stage", "--stage"])
                .arg(fixture.path("stage"))
                .args(&args),
        );
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{output:?}"
        );
        fixture.assert_inert();
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn native_fixture() -> Fixture {
    assert!(Path::new("/dev/kvm").exists(), "requires /dev/kvm");
    let mut fixture = Fixture::new();
    fs::create_dir(fixture.path("bin")).unwrap();
    fixture.binary = fixture.path("bin/pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &fixture.binary).unwrap();
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBRARY_DIR") {
        let directory = PathBuf::from(directory);
        let firmware = ["libkrunfw.so.5", "libkrunfw.so.5.6.2"]
            .into_iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
            .expect("PVISOR_TEST_LIBRARY_DIR must contain libkrunfw.so.5 or libkrunfw.so.5.6.2");
        fs::copy(firmware, fixture.path("bin/libkrunfw.so.5")).unwrap();
    }
    fixture
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM host-root VM; GNU builds need PVISOR_TEST_LIBRARY_DIR; run with --run-ignored only"]
fn native_host_root_stage_preserves_host_and_supports_review_and_drop() {
    let fixture = native_fixture();
    if std::env::var_os("PVISOR_TEST_STAGE_DIAGNOSTICS").is_some() {
        for (name, args) in [
            (
                "spawn-true",
                vec![
                    "spawn",
                    "--executor",
                    "vm",
                    "--rootfs",
                    "host",
                    "--overlaynet",
                    "off",
                    "--cpu",
                    "2",
                    "--memory",
                    "512MiB",
                    "--no-agent-defaults",
                ],
            ),
            ("stage-true", vec!["stage", "--network", "none"]),
            // Diagnostic only: explicit TMPDIR suppresses automatic scratch setup.
            // The acceptance workload below deliberately retains the default path.
            (
                "stage-no-scratch",
                vec!["stage", "--network", "none", "-e", "TMPDIR=/tmp"],
            ),
        ] {
            let mut command = fixture.command();
            command
                .env("PVISOR_STARTUP_PROFILE", "1")
                .env("PVISOR_FS_PROFILE", "1")
                .args(args);
            if name.starts_with("stage-") {
                command.arg("--stage").arg(fixture.path(name));
            }
            let output = fixture.run(command.args(["--", "/bin/true"]));
            eprintln!("diagnostic {name}: exit={:?}", output.status.code());
            for line in String::from_utf8_lossy(&output.stderr).lines() {
                if line.contains("\"role\":\"guest\"") || line.starts_with("pvisor guest:") {
                    eprintln!("{line}");
                }
            }
        }
    }
    // Siblings outside cwd exercise the entire host-root lower, not just workspace staging.
    let workspace = fixture.path("workspace");
    let system = fixture.path("system/etc");
    fs::create_dir_all(&system).unwrap();
    let workspace_file = workspace.join("state");
    let system_file = system.join("state");
    let guest_only = fixture.path("guest-only");
    fs::write(&workspace_file, "host-workspace").unwrap();
    fs::write(&system_file, "host-system").unwrap();
    let script = r#"set -eu
read -r input
[ "$input" = stage-stdin ]
[ "$PWD" = "$EXPECTED_CWD" ]
[ "$STAGE_ENV" = 'value with spaces' ]
[ "$1" = --cpus ] && [ "$2" = 99 ] && [ "$3" = --network ] && [ "$4" = host ]
printf guest-workspace > state
printf guest-system > "$SYSTEM_FILE"
printf guest-root > "$GUEST_ONLY"
printf 'stage-guest-ok\n'
exit 23
"#;
    let input = tempfile::tempfile().unwrap();
    {
        use std::io::Write;
        (&input).write_all(b"stage-stdin\n").unwrap();
        use std::io::{Seek, SeekFrom};
        (&input).seek(SeekFrom::Start(0)).unwrap();
    }
    let output = fixture.run(
        fixture
            .command()
            .args([
                "stage",
                "-i",
                "--network",
                "none",
                "--cpus",
                "2",
                "-m",
                "512m",
                "-w",
            ])
            .arg(&workspace)
            .arg("--stage")
            .arg(fixture.path("stage"))
            .args(["-e", "STAGE_ENV=value with spaces", "-e"])
            .arg(format!("EXPECTED_CWD={}", workspace.display()))
            .arg("-e")
            .arg(format!("SYSTEM_FILE={}", system_file.display()))
            .arg("-e")
            .arg(format!("GUEST_ONLY={}", guest_only.display()))
            .args([
                "/bin/sh",
                "-c",
                script,
                "stage-test",
                "--cpus",
                "99",
                "--network",
                "host",
            ])
            .stdin(input),
    );
    let assert_host = |workspace_content: &[u8]| {
        assert_eq!(fs::read(&workspace_file).unwrap(), workspace_content);
        assert_eq!(fs::read(&system_file).unwrap(), b"host-system");
        assert!(!guest_only.exists());
    };
    assert_host(b"host-workspace");
    if output.status.code() != Some(23) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut context = String::new();
        for path in [PathBuf::from("/"), fixture.path("stage/upper")] {
            if let Ok(metadata) = fs::metadata(&path) {
                context.push_str(&format!(
                    "{}: mode={:04o} uid={} gid={}\n",
                    path.display(),
                    metadata.permissions().mode() & 0o7777,
                    metadata.uid(),
                    metadata.gid(),
                ));
            }
        }
        eprintln!("native failure filesystem context:\n{context}");
        if std::env::var_os("PVISOR_TEST_KEEP_ARTIFACTS").is_some() {
            fs::write(fixture.path("native-context.log"), context).unwrap();
        }
    }
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("stage-guest-ok"),
        "{output:?}"
    );
    let review = || {
        let output = fixture.run(
            fixture
                .command()
                .arg("review")
                .arg(fixture.path("stage"))
                .arg("--json"),
        );
        success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("review JSON")
    };
    let bundle = review();
    assert_eq!(bundle["run"]["exit_code"], 23);
    assert_eq!(bundle["filesystem"]["target"], "/");
    let upper = PathBuf::from(bundle["filesystem"]["upper"].as_str().unwrap());
    for (path, content) in [
        (&workspace_file, "guest-workspace"),
        (&system_file, "guest-system"),
        (&guest_only, "guest-root"),
    ] {
        let relative = path.strip_prefix("/").unwrap();
        assert_eq!(fs::read(upper.join(relative)).unwrap(), content.as_bytes());
        assert!(
            bundle["filesystem"]["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["path"].as_str() == relative.to_str()),
            "missing change: {path:?}: {bundle}"
        );
    }
    use std::os::unix::fs::MetadataExt;
    let workspace_metadata = fs::metadata(&workspace).unwrap();
    let scope = &bundle["project_apply_scope"];
    assert_eq!(scope["version"], 1, "missing launch contract: {bundle}");
    assert_eq!(scope["root"], workspace.to_str().unwrap());
    assert_eq!(scope["device"], workspace_metadata.dev());
    assert_eq!(scope["inode"], workspace_metadata.ino());
    let stage = fixture.path("stage");
    let record_before = fs::read(stage.join("run.json")).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&record_before).unwrap();
    assert_eq!(record["workspace"], workspace.to_str().unwrap());
    assert_eq!(&record["project_apply_scope"], scope);
    let assert_no_ledger = || {
        assert!(!stage.join("apply-ledger.json").exists());
        assert!(
            pvisor_overlay_core::apply::load_apply_records(&stage)
                .unwrap()
                .is_empty()
        );
    };
    assert_no_ledger();
    // Never issue a rootfs execution request, even if authority checks regress.
    if unsafe { libc::geteuid() } != 0 {
        let refused = fixture.run(
            fixture
                .command()
                .arg("apply")
                .arg(&stage)
                .args(["--rootfs", "--path"])
                .arg(system_file.strip_prefix("/").unwrap())
                .args(["--dry-run", "--json"]),
        );
        assert_eq!(refused.status.code(), Some(3), "{refused:?}");
        let refusal: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
        assert_eq!(refusal["status"], "blocked", "{refusal}");
        assert!(refusal["execution"].is_null());
        assert!(
            refusal["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker
                    .as_str()
                    .is_some_and(|text| text.contains("requires effective UID 0"))),
            "{refusal}"
        );
        assert_host(b"host-workspace");
        assert_no_ledger();
    }
    let preview = fixture.run(fixture.command().arg("apply").arg(&stage).args([
        "--path",
        "state",
        "--dry-run",
        "--json",
    ]));
    success(&preview);
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["status"], "ready", "{preview}");
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["scope"]["kind"], "project");
    assert_eq!(preview["scope"]["target"], workspace.to_str().unwrap());
    assert_eq!(preview["selected"].as_array().unwrap().len(), 1);
    assert_eq!(preview["selected"][0]["path"], "state");
    assert!(preview["execution"].is_null());
    assert_host(b"host-workspace");
    assert_no_ledger();
    assert_eq!(fs::read(stage.join("run.json")).unwrap(), record_before);
    assert_eq!(
        review()["filesystem"],
        bundle["filesystem"],
        "preview changed stage"
    );

    // The sole writeback target is this fixture's workspace/state, never '/'.
    let applied = fixture.run(
        fixture
            .command()
            .arg("apply")
            .arg(&stage)
            .args(["--path", "state", "--json"]),
    );
    success(&applied);
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(applied["status"], "applied", "{applied}");
    assert_eq!(applied["scope"]["kind"], "project");
    assert_eq!(applied["scope"]["target"], workspace.to_str().unwrap());
    assert_eq!(applied["execution"]["applied"], 1);
    assert_eq!(applied["execution"]["ledger_confirmed"], true);
    assert_eq!(applied["execution"]["recovery_required"], false);
    assert_host(b"guest-workspace");
    let ledger = pvisor_overlay_core::apply::load_apply_records(&stage).unwrap();
    assert_eq!(ledger.len(), 1);
    assert!(
        ledger[0]
            .changes
            .iter()
            .all(|change| change.relative_path() == workspace_file.strip_prefix("/").unwrap())
    );
    let remaining = review();
    let changes = remaining["filesystem"]["changes"].as_array().unwrap();
    assert!(!changes.iter().any(
        |change| change["path"].as_str() == workspace_file.strip_prefix("/").unwrap().to_str()
    ));
    for (path, content) in [(&system_file, "guest-system"), (&guest_only, "guest-root")] {
        let relative = path.strip_prefix("/").unwrap();
        assert_eq!(fs::read(upper.join(relative)).unwrap(), content.as_bytes());
        assert!(
            changes
                .iter()
                .any(|change| change["path"].as_str() == relative.to_str()),
            "missing retained change: {remaining}"
        );
    }
    let dropped = fixture.run(fixture.command().arg("drop").arg(&stage));
    success(&dropped);
    assert_host(b"guest-workspace");
    let output = fixture.run(
        fixture
            .command()
            .arg("status")
            .arg(fixture.path("stage"))
            .arg("--json"),
    );
    success(&output);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["filesystem"]["state"], "discarded");
    assert_eq!(status["filesystem"]["changed_files"], 0);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn guest_environment(
    fixture: &Fixture,
    name: &str,
    args: &[&str],
    unset: &[&str],
) -> std::collections::BTreeMap<String, String> {
    let mut command = fixture.command();
    // These are deliberately fake values; no cache or S3 operation is requested.
    command
        .env("PVISOR_CACHE_TOKEN", "fake-host-cache-token")
        .env("AWS_ACCESS_KEY_ID", "fake-host-key")
        .env("AWS_SECRET_ACCESS_KEY", "fake-host-secret")
        .env("AWS_SESSION_TOKEN", "fake-host-session")
        .env(
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            fixture.path("nonexistent-fake-token"),
        );
    if name != "cache-only" {
        command.env("PVISOR_CACHE_BACKEND", "s3");
    }
    for key in unset {
        command.env_remove(key);
    }
    let output = fixture.run(
        command
            .args(["stage", "--network", "none", "--stage"])
            .arg(fixture.path(name))
            .args(args)
            .args(["--", "/usr/bin/env", "-0"]),
    );
    success(&output);
    let env = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = std::str::from_utf8(entry).unwrap();
            let (key, value) = entry.split_once('=').expect("guest environment entry");
            (key.to_owned(), value.to_owned())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(env.get("PVISOR_STAGE").map(String::as_str), Some("1"));
    env
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM host-root VM; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_scrubs_implicit_cache_and_s3_credentials_before_explicit_env() {
    let fixture = native_fixture();
    let cache_only = guest_environment(&fixture, "cache-only", &[], &[]);
    assert!(
        !cache_only.contains_key("PVISOR_CACHE_TOKEN"),
        "{cache_only:?}"
    );
    // Stage may project only baseline host variables; the security contract is
    // absence of implicit credentials, not inheritance of unrelated AWS values.
    let implicit = guest_environment(&fixture, "implicit-s3", &[], &[]);
    assert!(!implicit.contains_key("PVISOR_CACHE_TOKEN"), "{implicit:?}");
    assert!(
        !implicit.contains_key("PVISOR_CACHE_BACKEND"),
        "{implicit:?}"
    );
    assert!(
        !implicit.keys().any(|key| key.starts_with("AWS_")),
        "{implicit:?}"
    );
    let explicit = guest_environment(
        &fixture,
        "explicit-s3",
        &["-e", "AWS_SECRET_ACCESS_KEY=fake-explicit-secret"],
        &[],
    );
    assert_eq!(
        explicit.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
        Some("fake-explicit-secret")
    );
    for key in [
        "PVISOR_CACHE_TOKEN",
        "PVISOR_CACHE_BACKEND",
        "AWS_ACCESS_KEY_ID",
        "AWS_SESSION_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
    ] {
        assert!(
            !explicit.contains_key(key),
            "unexpected {key}: {explicit:?}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM host-root VM; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_explicit_unsets_survive_guest_defaults_and_scratch_setup() {
    let fixture = native_fixture();
    // A bare -e NAME with no host value is Docker's explicit unset, not NAME=.
    let env = guest_environment(
        &fixture,
        "unset",
        &["-e", "HOME", "-e", "PATH", "-e", "TMPDIR"],
        &["HOME", "PATH", "TMPDIR"],
    );
    for key in ["HOME", "PATH", "TMPDIR"] {
        assert!(
            !env.contains_key(key),
            "guest restored explicit unset {key}: {env:?}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn native_stage_host_pty(flags: &[&str], interactive: bool, tty: bool) {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
    };
    let fixture = native_fixture();
    let (mut master_fd, mut slave_fd) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let current = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(current >= 0);
    assert_eq!(
        unsafe {
            libc::fcntl(
                master.as_raw_fd(),
                libc::F_SETFL,
                current | libc::O_NONBLOCK,
            )
        },
        0
    );
    // All three host streams are real terminal descriptors, even without -t.
    let script = r#"set -eu
for fd in 0 1 2; do
    if [ -t "$fd" ]; then actual=tty; else actual=pipe; fi
    expected="$1"
    # Closed stdin stays non-TTY even when -t attaches stdout/stderr to a PTY.
    if [ "$fd" = 0 ] && [ "$2" = eof ]; then expected=pipe; fi
    [ "$actual" = "$expected" ] || { printf 'BAD_TTY:%s:%s\n' "$fd" "$actual"; exit 81; }
done
printf 'GUEST_TTY_OK\n'
if [ "$2" = input ]; then
    printf 'GUEST_READ_READY\n'
    read -r value
    [ "$value" = stage-pty-input ] || exit 82
    printf 'GUEST_INPUT_OK\n'
else
    if read -r value; then printf 'UNEXPECTED_INPUT\n'; exit 83; fi
    printf 'GUEST_EOF_OK\n'
fi
"#;
    let mut command = fixture.command();
    command
        .env("TERM", "xterm")
        .args(["stage", "--network", "none", "--stage"])
        .arg(fixture.path("stage"))
        .args(flags)
        .args([
            "--",
            "/bin/sh",
            "-c",
            script,
            "stage-pty",
            if tty { "tty" } else { "pipe" },
            if interactive { "input" } else { "eof" },
        ])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if (0..3).any(|fd| libc::isatty(fd) != 1) {
                return Err(std::io::Error::from_raw_os_error(libc::ENOTTY));
            }
            if libc::tcgetpgrp(0) != libc::getpgrp() {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn CLI with controlling PTY");
    drop(command);
    drop(slave);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut transcript = Vec::new();
    let mut sent_input = false;
    let status = loop {
        let mut buffer = [0; 4096];
        match master.read(&mut buffer) {
            Ok(size) => transcript.extend_from_slice(&buffer[..size]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("PTY read: {error}"),
        }
        if interactive
            && !sent_input
            && String::from_utf8_lossy(&transcript).contains("GUEST_READ_READY")
        {
            master
                .write_all(b"stage-pty-input\n")
                .expect("write PTY input");
            sent_input = true;
        }
        if let Some(status) = child.try_wait().unwrap() {
            // Drain bytes queued before exit without waiting for inherited descriptors.
            while let Ok(size) = master.read(&mut buffer) {
                if size == 0 {
                    break;
                }
                transcript.extend_from_slice(&buffer[..size]);
                if Instant::now() >= deadline {
                    break;
                }
            }
            break status;
        }
        if Instant::now() >= deadline || transcript.len() > 256 * 1024 {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            child.kill().ok();
            child.wait().unwrap();
            if std::env::var_os("PVISOR_TEST_KEEP_ARTIFACTS").is_some() {
                fs::write(fixture.path("pty.log"), &transcript).unwrap();
            }
            panic!(
                "PTY exceeded deadline/output bound ({flags:?}): {}",
                String::from_utf8_lossy(&transcript)
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    if std::env::var_os("PVISOR_TEST_KEEP_ARTIFACTS").is_some() {
        fs::write(fixture.path("pty.log"), &transcript).unwrap();
    }
    let transcript = String::from_utf8_lossy(&transcript);
    assert!(status.success(), "{flags:?}: {status}: {transcript}");
    assert!(transcript.contains("GUEST_TTY_OK"), "{transcript}");
    assert_eq!(sent_input, interactive, "{transcript}");
    assert!(
        transcript.contains(if interactive {
            "GUEST_INPUT_OK"
        } else {
            "GUEST_EOF_OK"
        }),
        "{transcript}"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM and host controlling PTY; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_explicit_command_without_t_has_non_tty_streams_and_eof() {
    native_stage_host_pty(&[], false, false);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM and host controlling PTY; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_i_forwards_host_pty_input_without_guest_tty() {
    native_stage_host_pty(&["-i"], true, false);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM and host controlling PTY; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_t_allocates_guest_tty_but_stdin_is_eof_without_i() {
    native_stage_host_pty(&["-t"], false, true);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "real Linux KVM and host controlling PTY; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_stage_it_allocates_guest_tty_and_forwards_input() {
    native_stage_host_pty(&["-it"], true, true);
}

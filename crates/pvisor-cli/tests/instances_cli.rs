//! Creation mode owns lifetime; all backends share ps, selection and removal.
#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Fixture {
    root: tempfile::TempDir,
}

fn staging_available() -> bool {
    #[cfg(target_os = "macos")]
    if !Path::new("/Library/Filesystems/macfuse.fs").is_dir() {
        eprintln!(
            "staged-write integration requires installed macFUSE; metadata lifetime tests still run"
        );
        return false;
    }
    true
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command
            .current_dir(self.path("workspace"))
            .env("HOME", self.root.path())
            .env("PVISOR_RUN_HOME", self.path("runs"))
            .env("PVISOR_DOCKER_HOME", self.path("docker"))
            .env("PVISOR_IMAGE_STORE", self.path("images"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        output
    }
}

#[test]
fn spawn_removes_default_and_explicit_storage_on_success_and_nonzero_exit() {
    let fixture = Fixture::new();
    for code in [0, 7] {
        for explicit in [false, true] {
            if explicit && !staging_available() {
                continue;
            }
            let stage = fixture.path("temporary-stage");
            let mut command = fixture.command();
            command.args(["spawn", "--no-agent-defaults", "--overlaynet", "off"]);
            if explicit {
                command.arg("--stage").arg(&stage);
            }
            let output = command
                .args(["--", "/bin/sh", "-c", &format!("exit {code}")])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(code), "{output:?}");
            assert!(!stage.exists());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("Review:"));
            assert!(fixture.success(&["ps", "-aq"]).stdout.is_empty());
            assert!(!fixture.run(&["review", "last"]).status.success());
        }
    }
}

#[test]
fn spawn_is_visible_while_running_and_cancellation_removes_its_stage() {
    use std::io::{BufRead, BufReader};
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .args([
            "spawn",
            "--no-agent-defaults",
            "--filesystem",
            "host",
            "--overlaynet",
            "off",
        ])
        .args(["--", "/bin/sh", "-c", "echo READY; exec /bin/sleep 30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "READY\n");
    let id = String::from_utf8(fixture.success(&["ps", "-q", "--no-trunc"]).stdout).unwrap();
    let stage = fixture.path("runs").join(id.trim());
    let listing = fixture.success(&["ps", "--no-trunc"]);
    let listing = String::from_utf8(listing.stdout).unwrap();
    let record = pvisor::RunRecord::read(&stage).unwrap();
    assert!(listing.contains(&record.run_id), "{listing}");
    assert!(listing.contains("\tspawn\t"), "{listing}");
    assert!(listing.contains("\tephemeral\t"), "{listing}");
    // rm/fork must not snapshot or delete a changing upper.
    assert!(
        !fixture
            .run(&["rm", record.run_id.as_str()])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["fork", record.run_id.as_str()])
            .status
            .success()
    );
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(!stage.exists());
    assert!(fixture.success(&["ps", "-aq"]).stdout.is_empty());
}

#[test]
fn custom_persistent_stage_is_listed_by_id_and_name_until_explicit_removal() {
    if !staging_available() {
        return;
    }
    let fixture = Fixture::new();
    let stage = fixture.path("outside-run-home");
    let output = fixture
        .command()
        .args([
            "stage",
            "--executor",
            "host",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--name",
            "experiment",
            "--stage",
        ])
        .arg(&stage)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf staged > result; printf discard > scratch; exit 7",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let record = pvisor::RunRecord::read(&stage).unwrap();
    assert_eq!(
        fs::read(record.stage_dir().join("upper/result")).unwrap(),
        b"staged"
    );
    assert!(stage.join(pvisor::instance::MANIFEST).is_file());
    assert!(!stage.join("run.json").exists());
    assert!(record.stage_dir().starts_with(stage.join("attempts")));
    assert!(!fixture.path("workspace/result").exists());
    assert!(fixture.success(&["ps", "-q"]).stdout.is_empty());
    let listing = fixture.success(&["ps", "-a", "--no-trunc"]);
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains(&record.run_id), "{listing}");
    assert!(listing.contains("\tstage\t"), "{listing}");
    assert!(listing.contains("\tpersistent\t"), "{listing}");
    fixture.success(&["review", "experiment"]);
    fixture.success(&[
        "apply",
        "experiment",
        "--path",
        "result",
        "--dry-run",
        "--json",
    ]);
    assert!(!fixture.path("workspace/result").exists());
    fixture.success(&["apply", "experiment", "--path", "result"]);
    assert_eq!(
        fs::read(fixture.path("workspace/result")).unwrap(),
        b"staged"
    );
    fixture.success(&["drop", "experiment"]);
    assert!(!fixture.path("workspace/scratch").exists());
    assert!(stage.exists(), "drop must not remove the instance");
    fixture.success(&["rm", "experiment"]);
    assert!(!stage.exists());
    assert!(fixture.success(&["ps", "-aq"]).stdout.is_empty());
    assert!(!fixture.run(&["rm", "experiment"]).status.success());
}

#[test]
fn reused_or_unowned_stage_paths_are_not_deleted() {
    let fixture = Fixture::new();
    let stage = fixture.path("existing");
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("unrelated"), b"keep").unwrap();
    let output = fixture
        .command()
        .args(["spawn", "--overlaynet", "off", "--stage"])
        .arg(&stage)
        .args(["--", "/usr/bin/true"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(stage.join("unrelated")).unwrap(), b"keep");
    assert!(
        !fixture
            .run(&["rm", stage.to_str().unwrap()])
            .status
            .success()
    );
    assert!(Path::new(&stage).exists());
}

#[test]
fn corrupt_instance_indexes_fail_closed_before_fork_or_removal() {
    let fixture = Fixture::new();
    fixture.success(&[
        "stage",
        "--executor",
        "host",
        "--overlaynet",
        "off",
        "--no-agent-defaults",
        "--name",
        "recorded",
        "--",
        "/usr/bin/true",
    ]);
    let index = fs::read_dir(fixture.path("runs/.pvisor/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original = fs::read(&index).unwrap();
    fs::write(&index, b"corrupt").unwrap();
    for args in [
        &["ps", "-aq"][..],
        &["fork", "recorded"],
        &["rm", "recorded"],
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid instance index"),
            "{output:?}"
        );
    }
    fs::write(index, original).unwrap();
    fixture.success(&["rm", "recorded"]);
}

#[test]
fn delegated_spawn_removes_explicit_storage_without_a_filesystem_mount() {
    let fixture = Fixture::new();
    let stage = fixture.path("delegated-stage");
    let spec_path = fixture.path("spec.json");
    let result_path = fixture.path("result.json");
    let mut spec = pvisor_core::RunSpec::process("delegated-lifetime", "sh", "/bin/sh");
    let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "exit 7".into()];
    fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let output = fixture
        .command()
        .args([
            "spawn",
            "--executor",
            "host",
            "--overlaynet",
            "off",
            "--filesystem",
            "host",
            "--spec",
        ])
        .arg(&spec_path)
        .arg("--result-file")
        .arg(&result_path)
        .arg("--stage")
        .arg(&stage)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(!stage.exists());
    assert!(result_path.is_file());
    assert!(fixture.success(&["ps", "-aq"]).stdout.is_empty());
}

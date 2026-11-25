#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    runs: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project with spaces");
        std::fs::create_dir(&workspace).unwrap();
        for name in ["tmp", "cache", "data", "runtime", "state"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let runs = root.path().join("runs");
        Self {
            root,
            workspace,
            runs,
        }
    }

    fn command(&self, cwd: &Path) -> Command {
        self.command_for(cwd, env!("CARGO_BIN_EXE_pvisor"))
    }

    fn command_for(&self, cwd: &Path, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("PVISOR_RUN_HOME", &self.runs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command(&self.workspace);
        command.args(args);
        finish(command)
    }

    fn spawn_failed_stage(&self, cwd: &Path, stage: &Path) {
        let mut command = self.command(cwd);
        command
            .args([
                "spawn",
                "--executor",
                "host",
                "--filesystem",
                "sandbox",
                "--overlaynet",
                "off",
                "--no-agent-defaults",
                "--stdio",
                "capture",
                "--timeout",
                "5s",
                "--stage",
            ])
            .arg(stage)
            .args(["--", "/bin/sh", "-c", "echo staged > candidate; exit 42"]);
        let output = finish(command);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(stage.join("upper/candidate")).unwrap(),
            b"staged\n"
        );
        assert!(!cwd.join("candidate").exists());
    }
}

fn finish(mut command: Command) -> Output {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI did not finish: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn last_finds_default_and_custom_storage_after_success_failure_and_timeout() {
    for (custom, script, timeout, exit) in [
        (false, "exit 0", "5s", 0),
        (true, "exit 0", "5s", 0),
        (true, "exit 42", "5s", 42),
        (true, "sleep 30", "200ms", 1),
    ] {
        let fixture = Fixture::new();
        let stage = fixture.root.path().join("outside run home/custom stage");
        let mut args = vec![
            "spawn",
            "--filesystem",
            "host",
            "--overlaynet",
            "off",
            "--no-agent-defaults",
            "--timeout",
            timeout,
            "--stdio",
            "capture",
        ];
        if custom {
            args.extend(["--stage", stage.to_str().unwrap()]);
        }
        args.extend(["--", "/bin/sh", "-c", script]);
        let output = fixture.run(&args);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let selected = fixture.run(&["status", "last", "--json"]);
        assert!(
            selected.status.success(),
            "{}",
            String::from_utf8_lossy(&selected.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
        assert_eq!(
            status["state"],
            if exit == 0 { "completed" } else { "failed" }
        );
        let id = status["run"]["run_id"].as_str().unwrap();
        if custom {
            assert_eq!(status["run"]["stage"], stage.to_str().unwrap());
        }
        assert!(fixture.run(&["review", "last", "--diff"]).status.success());
        assert!(fixture.run(&["status", id, "--json"]).status.success());
        let other_workspace = fixture.root.path().join("other workspace");
        std::fs::create_dir(&other_workspace).unwrap();
        let mut command = fixture.command(&other_workspace);
        command.args(["status", "last", "--json"]);
        let absent = finish(command);
        assert!(!absent.status.success());
        assert!(String::from_utf8_lossy(&absent.stderr).contains("current workspace"));
        let stored = PathBuf::from(status["run"]["stage"].as_str().unwrap());
        std::fs::remove_dir_all(stored).unwrap();
        assert!(!fixture.run(&["status", "last", "--json"]).status.success());
    }
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn last_prefers_the_nearest_workspace_and_its_latest_session() {
    let fixture = Fixture::new();
    let outer = &fixture.workspace;
    let inner = outer.join("inner");
    let inner_child = inner.join("src");
    let outer_child = outer.join("inner-sibling");
    std::fs::create_dir_all(&inner_child).unwrap();
    std::fs::create_dir(&outer_child).unwrap();

    let session_id = |cwd: &Path, selector: Option<&Path>| {
        let mut command = fixture.command(cwd);
        command.arg("status").args(selector).arg("--json");
        let output = finish(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        status["run"]["run_id"].as_str().unwrap().to_owned()
    };
    let spawn = |cwd: &Path, stage_name: &str| {
        let stage = fixture.root.path().join(stage_name);
        let mut command = fixture.command(cwd);
        command
            .args([
                "spawn",
                "--filesystem",
                "host",
                "--overlaynet",
                "off",
                "--no-agent-defaults",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(&stage)
            .args(["--", "/bin/sh", "-c", "exit 0"]);
        let output = finish(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        session_id(cwd, Some(&stage))
    };

    let older_inner = spawn(&inner, "older inner stage");
    let newer_inner = spawn(&inner, "newer inner stage");
    let outer_id = spawn(outer, "outer stage");
    assert_ne!(older_inner, newer_inner);

    let last = Some(Path::new("last"));
    assert_eq!(session_id(&inner, last), newer_inner);
    assert_eq!(session_id(&inner_child, None), newer_inner);
    assert_eq!(session_id(&outer_child, last), outer_id);
    assert_eq!(session_id(outer, last), outer_id);
    assert_eq!(session_id(&inner_child, Some(&fixture.runs)), outer_id);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn custom_live_session_is_discoverable_and_cancellable_through_last() {
    let fixture = Fixture::new();
    let stage = fixture.root.path().join("custom stage");
    let mut command = fixture.command(&fixture.workspace);
    command.args([
        "spawn",
        "--filesystem",
        "host",
        "--overlaynet",
        "off",
        "--no-agent-defaults",
        "--stage",
        stage.to_str().unwrap(),
        "--timeout",
        "10s",
        "--",
        "/bin/sh",
        "-c",
        "printf ready > ready; sleep 30",
    ]);
    let mut owner = command.spawn().unwrap();
    let ready = Instant::now() + Duration::from_secs(5);
    let selected = loop {
        let selected = fixture.run(&["status", "last", "--lifecycle", "--json"]);
        let running = selected.status.success()
            && serde_json::from_slice::<serde_json::Value>(&selected.stdout).unwrap()["state"]
                == "running";
        if running || owner.try_wait().unwrap().is_some() || Instant::now() >= ready {
            break selected;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let cancelled = if selected.status.success() {
        fixture.run(&["kill", "-s", "TERM", "last"])
    } else {
        fixture.run(&["kill", "-s", "TERM", stage.to_str().unwrap()])
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            owner.kill().unwrap();
            owner.wait().unwrap();
            panic!("cancelled Session frontend did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = owner.wait_with_output().unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(status["live"], true);
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    assert!(!output.status.success());
    let stopped = fixture.run(&["status", "last", "--json"]);
    assert!(stopped.status.success());
    let status: serde_json::Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(status["live"], false);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn failed_discovery_registration_drains_and_preserves_the_explicit_stage() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(&fixture.runs).unwrap();
    let obstruction = fixture.runs.join(".pvisor");
    std::fs::write(&obstruction, b"keep existing entry").unwrap();
    let stage = fixture.root.path().join("custom stage");
    let output = fixture.run(&[
        "spawn",
        "--filesystem",
        "host",
        "--overlaynet",
        "off",
        "--no-agent-defaults",
        "--stage",
        stage.to_str().unwrap(),
        "--timeout",
        "10s",
        "--",
        "/bin/sh",
        "-c",
        "sleep 30",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("discovery registration failed after launch"),
        "{error}"
    );
    assert!(error.contains("workload effects may remain"), "{error}");
    assert!(error.contains(stage.to_str().unwrap()), "{error}");
    assert_eq!(std::fs::read(obstruction).unwrap(), b"keep existing entry");
    let selected = fixture.run(&["status", stage.to_str().unwrap(), "--json"]);
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(status["live"], false);
    assert_ne!(status["state"], "running");
    assert!(stage.join("completion.json").is_file());
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn implicit_session_selection_does_not_follow_a_rebound_workspace_path() {
    let f = Fixture::new();
    let alias = f.root.path().join("launch-alias");
    let stage = f.root.path().join("external-stage");
    std::os::unix::fs::symlink(&f.workspace, &alias).unwrap();
    f.spawn_failed_stage(&alias, &stage);

    let mut command = f.command(&alias);
    command.args(["status", "last", "--json"]);
    let original = finish(command);
    assert!(original.status.success());
    let status: serde_json::Value = serde_json::from_slice(&original.stdout).unwrap();
    assert_eq!(status["state"], "failed");
    let id = status["run"]["run_id"].as_str().unwrap();

    let moved = f.root.path().join("project moved");
    let sibling = f.root.path().join("project with spaces clone");
    std::fs::rename(&f.workspace, &moved).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    std::os::unix::fs::symlink(&sibling, &f.workspace).unwrap();

    for cwd in [&alias, &sibling, &moved, &stage] {
        let mut command = f.command(cwd);
        command.args(["status", "last", "--json"]);
        let status = finish(command);
        assert!(
            !status.status.success(),
            "selected foreign Session: {status:?}"
        );
        assert!(String::from_utf8_lossy(&status.stderr).contains("current workspace"));
    }
    assert!(!f.run(&["status", "--json"]).status.success());
    assert!(!f.run(&["review", "last", "--diff"]).status.success());
    let preview = f.run(&["apply", "last", "--all", "--dry-run", "--json"]);
    assert_eq!(preview.status.code(), Some(1), "{preview:?}");
    let plan: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(plan["source"].is_null());

    let explicit = f.run(&["status", id, "--json"]);
    assert!(explicit.status.success(), "{explicit:?}");
    let status: serde_json::Value = serde_json::from_slice(&explicit.stdout).unwrap();
    assert_eq!(status["run"]["run_id"], id);
    let review = f.run(&["review", stage.to_str().unwrap(), "--diff"]);
    assert!(review.status.success(), "{review:?}");
    assert!(String::from_utf8_lossy(&review.stdout).contains("Added candidate"));
    assert!(!sibling.join("candidate").exists());
    assert!(!moved.join("candidate").exists());
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn explicit_apply_refuses_replaced_workspace_and_accepts_restored_directory() {
    for rebound_as_symlink in [false, true] {
        let f = Fixture::new();
        let root = f.root.path();
        let stage = root.join("external-stage");
        let moved = root.join("saved-project");
        let replacement = root.join("replacement-project");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("sentinel"), b"other checkout").unwrap();
        f.spawn_failed_stage(&f.workspace, &stage);
        std::fs::rename(&f.workspace, &moved).unwrap();
        if rebound_as_symlink {
            std::os::unix::fs::symlink(&replacement, &f.workspace).unwrap();
        } else {
            std::fs::rename(&replacement, &f.workspace).unwrap();
        }
        let implicit = f.run(&["status", "last", "--json"]);
        assert_eq!(implicit.status.code(), Some(1), "{implicit:?}");
        assert!(
            String::from_utf8_lossy(&implicit.stderr).contains("current workspace"),
            "{implicit:?}"
        );

        let apply = |preview| {
            let mut command = f.command(root);
            command
                .args(["apply"])
                .arg(&stage)
                .args(["--path", "candidate", "--json"]);
            if preview {
                command.arg("--dry-run");
            }
            let output = finish(command);
            let body = serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
            (output.status.code(), body)
        };
        for preview in [true, false] {
            let (exit, response) = apply(preview);
            assert_eq!(exit, Some(3), "{response}");
            assert_eq!(response["status"], "blocked");
            let blocker = response["blockers"][0].as_str().unwrap();
            assert!(
                blocker.contains("original project directory")
                    || blocker.contains("project apply root identity changed"),
                "{blocker}"
            );
            assert!(!f.workspace.join("candidate").exists());
            assert!(!moved.join("candidate").exists());
            assert_eq!(
                std::fs::read(f.workspace.join("sentinel")).unwrap(),
                b"other checkout"
            );
            assert_eq!(
                std::fs::read(stage.join("upper/candidate")).unwrap(),
                b"staged\n"
            );
            assert!(!stage.join("apply-ledger.json").exists());
        }

        if rebound_as_symlink {
            std::fs::remove_file(&f.workspace).unwrap();
        } else {
            std::fs::rename(&f.workspace, &replacement).unwrap();
        }
        std::fs::rename(&moved, &f.workspace).unwrap();
        assert!(f.run(&["status", "last", "--json"]).status.success());
        let (exit, preview) = apply(true);
        assert_eq!(exit, Some(0), "{preview}");
        assert_eq!(preview["status"], "ready");
        let (exit, applied) = apply(false);
        assert_eq!(exit, Some(0), "{applied}");
        assert_eq!(applied["status"], "applied");
        assert_eq!(
            std::fs::read(f.workspace.join("candidate")).unwrap(),
            b"staged\n"
        );
        assert!(!replacement.join("candidate").exists());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn apply_preview_does_not_select_a_session_when_cwd_disappears() {
    let f = Fixture::new();
    let stage = f.root.path().join("external-stage");
    f.spawn_failed_stage(&f.workspace, &stage);
    let unrelated = f.root.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    let mut command = f.command_for(&unrelated, "/bin/sh");
    command
        .args(["-c", "rmdir -- \"$1\" || exit 99; shift; exec \"$@\"", "sh"])
        .arg(&unrelated)
        .arg(env!("CARGO_BIN_EXE_pvisor"))
        .args(["apply", "last", "--all", "--dry-run", "--json"]);
    let preview = finish(command);
    assert_eq!(preview.status.code(), Some(1), "{preview:?}");
    let plan: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(plan["source"].is_null());
    assert!(
        plan["blockers"][0]
            .as_str()
            .unwrap()
            .contains("resolve current workspace")
    );
    assert!(!unrelated.exists());
    assert!(!f.workspace.join("candidate").exists());
}

use std::path::Path;
use std::process::{Command, Output};

fn invoke(args: &[&str], stage: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    if let Some(stage) = stage {
        let offset = if args[0] == "checkpoint" { 2 } else { 1 };
        command
            .args(&args[..offset])
            .arg(stage)
            .args(&args[offset..]);
    } else {
        command.args(args);
    }
    command.env("PATH", "").output().unwrap()
}
fn fixture(root: &Path) -> pvisor::RunRecord {
    let stage = root.join("stage");
    let target = root.join("target");
    std::fs::create_dir_all(stage.join("upper")).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(stage.join("upper/file"), b"staged").unwrap();
    let record: pvisor::RunRecord = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run_id":"job-test","session_id":"job-test","agent":"sh",
        "pid":0,"command":["/bin/sh"],"state":"completed", "started_at_unix_ms":1,
        "finished_at_unix_ms":2,"storage":stage,"network":{},"gateway_listen":null,
        "overlay":{"id":"job-test","generation":7,"target":target,
            "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
            "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
    }))
    .unwrap();
    record.write().unwrap();
    record
}
fn managed_fixture(root: &Path) -> pvisor::RunRecord {
    let stage = root.join("stage");
    let target = root.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = listener.local_addr().unwrap().to_string();
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args([
            "run",
            "--filesystem",
            "host",
            "--overlaynet-deny-all",
            "--overlaynet-listen",
        ])
        .arg(&listen)
        .args(["--gateway-mode", "off", "--stdio", "capture", "--stage"])
        .arg(&stage)
        .args(["--", "/bin/sh", "-c", "printf staged > file"])
        .current_dir(&target)
        .env("PVISOR_RUN_HOME", root.join("runs"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stage.join("workspace-launch-policy.json").is_file());
    assert_eq!(std::fs::read(stage.join("upper/file")).unwrap(), b"staged");
    assert!(!target.join("file").exists());
    pvisor::RunRecord::read(&stage).unwrap()
}

fn filesystem_snapshot(
    root: &Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(
        root: &Path,
        path: &Path,
        snapshot: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let is_dir = entry.file_type().unwrap().is_dir();
            snapshot.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                if is_dir {
                    None
                } else {
                    Some(std::fs::read(&path).unwrap())
                },
            );
            if is_dir {
                visit(root, &path, snapshot);
            }
        }
    }
    let mut snapshot = std::collections::BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn parsed(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn job_commands_are_builtins_and_mutations_need_explicit_selection() {
    let output = invoke(&["--help"], None);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for name in ["review", "checkpoint", "suspend", "resume"] {
        assert!(text.contains(&format!("  {name} ")), "{text}");
        assert!(invoke(&[name, "--help"], None).status.success());
    }
    for args in [
        &["apply"][..],
        &["drop"],
        &["kill"],
        &["fork"],
        &["checkpoint", "create"],
    ] {
        assert_eq!(invoke(args, None).status.code(), Some(2));
    }
    assert_eq!(
        invoke(&["fork", "source", "--state", "nonsense"], None)
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn workspace_management_retry_and_deletion_are_job_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let record = fixture(temp.path());
    let stage = record.stage_dir();
    let create = || {
        parsed(invoke(
            &[
                "checkpoint",
                "create",
                "--request-id",
                "my-request",
                "--json",
            ],
            Some(&stage),
        ))
    };
    let first = create();
    assert_eq!(first["reused"], false);
    assert_eq!(first["checkpoint"]["kind"], "workspace");
    assert_eq!(first["checkpoint"]["workspace_generation"], 7);
    let second = create();
    assert_eq!(second["reused"], true);
    assert_eq!(second["checkpoint_id"], first["checkpoint_id"]);
    let list = parsed(invoke(&["checkpoint", "list", "--json"], Some(&stage)));
    assert_eq!(list["checkpoints"].as_array().unwrap().len(), 1);
    let id = first["checkpoint_id"].as_str().unwrap();
    let shown = parsed(invoke(&["checkpoint", "show", "--json", id], Some(&stage)));
    assert_eq!(shown["branch_references"], 0);
    let deleted = parsed(invoke(
        &["checkpoint", "delete", "--json", id],
        Some(&stage),
    ));
    assert_eq!(deleted["deleted"], true);
    let retry = invoke(
        &[
            "checkpoint",
            "create",
            "--request-id",
            "my-request",
            "--json",
        ],
        Some(&stage),
    );
    assert_eq!(retry.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&retry.stderr).contains("already committed"));
    let gc = parsed(invoke(&["checkpoint", "gc", "--json"], Some(&stage)));
    assert_eq!(gc["scope"], "job_workspace_transactions");
}

#[test]
fn unsupported_execution_operations_cannot_change_the_job_or_capture_files() {
    let temp = tempfile::tempdir().unwrap();
    let record = fixture(temp.path());
    let stage = record.stage_dir();
    let original = std::fs::read(stage.join("run.json")).unwrap();
    for args in [
        &["suspend"][..],
        &["resume"],
        &["checkpoint", "create", "--kind", "execution"],
        &["fork", "--state", "execution"],
    ] {
        let output = invoke(args, Some(&stage));
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("CAPABILITY_UNSUPPORTED"));
        assert_eq!(std::fs::read(stage.join("run.json")).unwrap(), original);
        assert!(!stage.join("checkpoints").exists());
    }
    let status = parsed(invoke(&["status", "--json"], Some(&stage)));
    assert_eq!(status["checkpoint_capability"]["execution"], false);
    assert_eq!(status["workspace_generation"], 7);
    assert!(
        parsed(invoke(&["kill", "--json"], Some(&stage)))["already_stopped"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn legacy_workspace_fork_rejects_missing_policy_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let record = fixture(temp.path());
    let stage = record.stage_dir();
    let child = temp.path().join("child");
    // Admission creates the lease file even when launch policy validation rejects the fork.
    let (_, lease) = record.lock_current().unwrap();
    drop(lease);
    let original = filesystem_snapshot(temp.path());
    let fork = |checkpoint: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command.arg("fork");
        if let Some(id) = checkpoint {
            command.args(["--checkpoint", id]);
        }
        command
            .arg("--stage")
            .arg(&child)
            .arg(&stage)
            .args(["--", "/bin/sh", "-c", "printf child > file"])
            .env("PVISOR_RUN_HOME", temp.path().join("runs"))
            .output()
            .unwrap()
    };
    let assert_rejected = |output: Output| {
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("requires persisted runtime-resolved launch policy"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!child.exists());
    };
    assert_rejected(fork(None));
    assert_eq!(filesystem_snapshot(temp.path()), original);
    assert!(!stage.join("checkpoints").exists());

    let checkpoint = parsed(invoke(&["checkpoint", "create", "--json"], Some(&stage)));
    let id = checkpoint["checkpoint_id"].as_str().unwrap();
    let original = filesystem_snapshot(temp.path());
    assert_rejected(fork(Some(id)));
    assert_eq!(filesystem_snapshot(temp.path()), original);
    let show = parsed(invoke(&["checkpoint", "show", "--json", id], Some(&stage)));
    assert_eq!(show["branch_references"], 0);
}

#[test]
fn workspace_fork_copies_files_and_preimages_and_retains_source_checkpoint() {
    #[cfg(target_os = "macos")]
    if !Path::new("/Library/Filesystems/macfuse.fs").is_dir() {
        eprintln!("skipping workspace fork integration: macFUSE is not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let record = managed_fixture(temp.path());
    let stage = record.stage_dir();
    let checkpoint = parsed(invoke(&["checkpoint", "create", "--json"], Some(&stage)));
    let id = checkpoint["checkpoint_id"].as_str().unwrap();
    let child = temp.path().join("child");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["fork", "--checkpoint", id, "--stage"])
        .arg(&child)
        .arg(&stage)
        .args(["--", "/bin/sh", "-c", "printf child > file"])
        .env("PVISOR_RUN_HOME", temp.path().join("runs"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let child_record = pvisor::RunRecord::read(&child).unwrap();
    let child_listen: std::net::SocketAddr = child_record
        .overlaynet_listen
        .as_deref()
        .expect("fork retains the parent's OverlayNet proxy")
        .parse()
        .unwrap();
    assert!(child_listen.ip().is_loopback());
    assert_ne!(child_listen.port(), 0);
    assert_ne!(child_record.run_id, record.run_id);
    assert_eq!(child_record.lineage.as_ref().unwrap().checkpoint_id, id);
    assert_eq!(std::fs::read(child.join("upper/file")).unwrap(), b"child");
    assert_eq!(std::fs::read(stage.join("upper/file")).unwrap(), b"staged");
    assert!(child.join("preimages").is_dir());
    assert!(child.join("source-checkpoint.json").is_file());
    let show = parsed(invoke(&["checkpoint", "show", "--json", id], Some(&stage)));
    assert_eq!(show["branch_references"], 1);
    let delete = invoke(&["checkpoint", "delete", id], Some(&stage));
    assert_eq!(delete.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&delete.stderr).contains("CHECKPOINT_REFERENCED"));
    assert!(invoke(&["drop"], Some(&child)).status.success());
    assert!(child.join("source-checkpoint.json").is_file());
    assert!(stage.join("checkpoints").join(id).exists());
}

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
#[test]
fn status_review_unknown_is_structured_and_preserves_stage_and_apply_fence() {
    let root = tempfile::tempdir().unwrap();
    let mut record = fixture(root.path());
    record.schema_version = 2;
    record.completion_protocol = Some(1);
    record.attempt_id = Some("interrupted-attempt".into());
    record.write().unwrap();
    let stage = record.stage_dir();
    let bytes = std::fs::read(stage.join("upper/file")).unwrap();
    for args in [
        vec!["status", "--review", "--json"],
        vec!["review", "--json"],
    ] {
        let output = invoke(&args, Some(&stage));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["state"], "unknown");
        assert_eq!(value["code"], "COMPLETION_UNCOMMITTED");
        assert_eq!(value["stage_retained"], true);
        assert_eq!(value["workspace_mutation_allowed"], false);
        assert_eq!(value["recovery"]["command"], "status");
        assert!(value.get("safety").is_none());
    }
    let text = invoke(&["status", "--review"], Some(&stage));
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("outcome: UNKNOWN"));
    assert!(text.contains("pvisor status"));
    let status = invoke(&["status", "--json"], Some(&stage));
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(
        pvisor::RunRecord::read(&stage).unwrap().state,
        pvisor::RunRecordState::Unknown
    );
    let apply = invoke(&["apply", "--all"], Some(&stage));
    assert!(!apply.status.success());
    assert_eq!(std::fs::read(stage.join("upper/file")).unwrap(), bytes);
    assert!(!root.path().join("target/file").exists());
    assert!(!stage.join("completion.json").exists());
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
            "stage",
            "--executor",
            "host",
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
    let record = pvisor::RunRecord::read(&stage).unwrap();
    assert!(
        record
            .stage_dir()
            .join("workspace-launch-policy.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read(record.stage_dir().join("upper/file")).unwrap(),
        b"staged"
    );
    assert!(!target.join("file").exists());
    record
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
fn workspace_ask_fork_rejection_is_inert_and_retryable() {
    #[cfg(target_os = "macos")]
    if !Path::new("/Library/Filesystems/macfuse.fs").is_dir() {
        return;
    }
    for location in ["inherited", "recorded", "checkpoint"] {
        let temp = tempfile::tempdir().unwrap();
        let mut record = managed_fixture(temp.path());
        let stage = record.stage_dir();
        let ask = pvisor_core::FileAccessPolicy::layered(
            [(
                pvisor_core::PolicyScope::User,
                &pvisor_core::FileAccessPolicy::new_with_ask(vec![], vec!["secret".into()], vec![])
                    .unwrap(),
            )],
            &Default::default(),
        );
        let checkpoint = if location == "checkpoint" {
            let checkpoint = parsed(invoke(&["checkpoint", "create", "--json"], Some(&stage)));
            let id = checkpoint["checkpoint_id"].as_str().unwrap().to_owned();
            let manifest = stage.join("checkpoints").join(&id).join("checkpoint.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            value["access_policy"] = serde_json::to_value(&ask).unwrap();
            std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
            Some(id)
        } else {
            if location == "inherited" {
                let path = stage.join("workspace-launch-policy.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                value["policies"]["session"]["filesystem"] = serde_json::to_value(&ask).unwrap();
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            } else {
                record.overlay.as_mut().unwrap().access_policy = ask;
                record.write().unwrap();
            }
            None
        };
        let (_, lease) = record.lock_current().unwrap();
        drop(lease);
        // Admission lock files are not checkpoint/stage mutations.
        std::fs::write(record.instance_dir().unwrap().join("job.lock"), []).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            record.instance_dir().unwrap().join("job.lock"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let original = filesystem_snapshot(temp.path());
        let child = temp.path().join("child");
        for _ in 0..2 {
            let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
            command.arg("fork");
            if let Some(id) = &checkpoint {
                command.args(["--checkpoint", id]);
            }
            let output = command
                .arg("--stage")
                .arg(&child)
                .arg(&stage)
                .output()
                .unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("ask file policies"), "{location}: {error}");
            assert!(!child.exists());
            let after = filesystem_snapshot(temp.path());
            assert_eq!(
                after.keys().collect::<Vec<_>>(),
                original.keys().collect::<Vec<_>>()
            );
            for (path, before) in &original {
                assert!(
                    after.get(path) == Some(before),
                    "{location}: changed {}",
                    path.display()
                );
            }
        }
        if let Some(id) = checkpoint {
            let show = parsed(invoke(&["checkpoint", "show", "--json", &id], Some(&stage)));
            assert_eq!(show["branch_references"], 0);
        } else {
            assert!(!stage.join("checkpoints").exists());
        }
    }
}

#[test]
fn workspace_fork_clones_files_and_preimages_without_retaining_parent_storage() {
    #[cfg(target_os = "macos")]
    if !Path::new("/Library/Filesystems/macfuse.fs").is_dir() {
        eprintln!("skipping workspace fork integration: macFUSE is not installed");
        return;
    }
    let temp = tempfile::Builder::new()
        .prefix("pvisor-cow-fork-")
        .tempdir_in(
            std::env::var_os("PVISOR_TEST_COW_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "/var/tmp".into()),
        )
        .unwrap();
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
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("CAPABILITY_UNSUPPORTED: CoW clone")
    {
        assert!(
            !child.exists(),
            "unsupported CoW must not publish a partial child"
        );
        assert_eq!(std::fs::read(stage.join("upper/file")).unwrap(), b"staged");
        let show = parsed(invoke(&["checkpoint", "show", "--json", id], Some(&stage)));
        assert_eq!(show["branch_references"], 0);
        assert!(
            std::env::var_os("PVISOR_TEST_REQUIRE_COW").is_none(),
            "{output:?}"
        );
        eprintln!(
            "filesystem has no native CoW; refusal, source preservation and pin cleanup verified"
        );
        return;
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let child_record = pvisor::RunRecord::read(&child).unwrap();
    let child_stage = child_record.stage_dir();
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
    assert_eq!(
        std::fs::read(child_stage.join("upper/file")).unwrap(),
        b"child"
    );
    assert_eq!(std::fs::read(stage.join("upper/file")).unwrap(), b"staged");
    assert!(child_stage.join("preimages").is_dir());
    assert!(!child_stage.join("source-checkpoint.json").exists());
    let show = parsed(invoke(&["checkpoint", "show", "--json", id], Some(&stage)));
    assert_eq!(show["branch_references"], 0);
    let delete = invoke(&["checkpoint", "delete", id], Some(&stage));
    assert!(delete.status.success(), "{delete:?}");
    assert!(!stage.join("checkpoints").join(id).exists());
    assert!(invoke(&["rm"], Some(&stage)).status.success());
    assert!(!stage.exists());
    assert_eq!(
        std::fs::read(child_stage.join("upper/file")).unwrap(),
        b"child"
    );
    assert!(invoke(&["drop"], Some(&child)).status.success());
    assert!(invoke(&["rm"], Some(&child)).status.success());
    assert!(!child.exists());
}

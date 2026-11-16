use super::*;
use fixtures::{AGENTS, COMMAND, NEXT_COMMAND, OBSERVATION, SENTINEL};

fn manifest(agent: &str) -> Value {
    json!({
        "agent":{"name":agent, "profile":"test"},
        "source":{"sha256":"source"},
        "boundary":{"after_step":1, "tool_calls":1, "complete_tool_batch":true},
        "batches":[{"tool_calls":[{"arguments":{"command":COMMAND}}]}]
    })
}

fn prefix() -> Value {
    json!([{"command":COMMAND, "observation":OBSERVATION}])
}

#[test]
fn native_fixture_documents_match_original_python_contract() {
    use sha2::{Digest, Sha256};
    // SHA-256 of the original replay_fixtures.py task-0 document serialized
    // with sorted keys and compact JSON separators. Embedded strings are not
    // normalized: Codex's argument string, IDs, versions and all native fields
    // must remain exact. These are fixture goldens, not measurement data.
    let expected = [
        "54bc4449ec1819af1d4a00b43ca53e4a9dae1369501013f260c4b67c2ce49a78",
        "ca3caf0347065235a9b9165055b597af5f70d0094c1abef59a7fe281a1e76596",
        "42f2ebcc4ebcd942cae42e838fbd981bfbb2bcb35e9ac9dd2d5dbe64ef19df59",
        "abb5eb7223ebc5e9852bb407fa93501157152dc7956e08d329e2a47311f012da",
        "4a7274685d60cfa652056f8b2300350132776500e45d7a7a06a2a56f0ddad07a",
        "a425354e7486918878fdf68e3c7e624a0e982532ec12ab95f8afb515e952d2cc",
        "795b9e2738d5677387d2a055a1c06afbbffa2ddf4e25c85e03786fa1f69e9d83",
    ];
    for (agent, expected) in AGENTS.into_iter().zip(expected) {
        let document = fixtures::trajectory(agent).0;
        let bytes = serde_json::to_vec(&document).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected, "{agent}");
    }
}

#[test]
fn all_fixtures_preserve_two_native_actions_and_historical_observation() {
    for agent in AGENTS {
        let (value, jsonl) = fixtures::trajectory(agent);
        let encoded = fixtures::encoded(agent);
        let decoded: Value = if jsonl {
            Value::Array(
                encoded
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect(),
            )
        } else {
            serde_json::from_str(&encoded).unwrap()
        };
        assert_eq!(decoded, value, "{agent}");
        assert_eq!(
            jsonl,
            !matches!(agent, "swe-agent" | "openhands" | "mini-swe-agent")
        );
        let mut strings = Vec::new();
        fixtures::nested_strings(&decoded, &mut strings);
        for required in [COMMAND, NEXT_COMMAND, OBSERVATION] {
            assert!(
                strings.iter().any(|s| s == required),
                "{agent}: missing {required}"
            );
        }
    }
    let codex = fixtures::trajectory("codex").0;
    assert_eq!(
        codex[3]["payload"]["arguments"],
        format!("{{\"cmd\": \"{COMMAND}\"}}")
    );
    let claude = fixtures::trajectory("claude-code").0;
    assert_eq!(claude[3]["parentUuid"], "result-0");
    assert_eq!(claude[4]["sourceToolAssistantUUID"], "assistant-1");
    let swe = fixtures::trajectory("swe-agent").0;
    assert_eq!(swe["history"].as_array().unwrap().len(), 5);
    assert_eq!(swe["trajectory"].as_array().unwrap().len(), 2);
}

#[test]
fn fidelity_rejects_wrong_source_adapter_boundary_and_arguments() {
    for agent in AGENTS {
        let valid = manifest(agent);
        checks::validate_prefix(&valid, &prefix(), agent, "source").unwrap();
        for (pointer, replacement) in [
            ("/agent/name", json!("other")),
            ("/source/sha256", json!("changed")),
            ("/boundary/after_step", json!(2)),
            ("/boundary/tool_calls", json!(2)),
            ("/boundary/complete_tool_batch", json!(false)),
            ("/batches", json!([])),
            ("/batches/0/tool_calls", json!([])),
            (
                "/batches/0/tool_calls/0/arguments/command",
                json!(NEXT_COMMAND),
            ),
        ] {
            let mut bad = valid.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                checks::validate_prefix(&bad, &prefix(), agent, "source").is_err(),
                "{agent} {pointer}"
            );
        }
        for native in [
            json!({"command":COMMAND}),
            json!({"observation":OBSERVATION}),
            json!({"command":COMMAND, "observation":OBSERVATION, "next":NEXT_COMMAND}),
        ] {
            assert!(checks::validate_prefix(&valid, &native, agent, "source").is_err());
        }
        // Codex embeds command JSON in a string.
        checks::validate_prefix(
            &valid,
            &json!({"arguments":format!("{{\"cmd\":\"{COMMAND}\"}}"), "output":OBSERVATION}),
            agent,
            "source",
        )
        .unwrap();
    }
}

fn workspace(root: &Path) -> String {
    fs::create_dir_all(root.join("workspace")).unwrap();
    fs::write(root.join("workspace/existing"), SENTINEL).unwrap();
    fs::write(root.join("trajectory.json"), fixtures::encoded("codex")).unwrap();
    checks::digest(&root.join("trajectory.json")).unwrap()
}

#[test]
fn reset_guards_source_workspace_and_unaudited_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let sha = workspace(root);
    checks::prepare(root, &sha).unwrap();
    for name in ["state", "output"] {
        fs::create_dir(root.join(name)).unwrap();
        assert!(checks::prepare(root, &sha).is_err());
        fs::remove_dir(root.join(name)).unwrap();
    }
    fs::write(root.join("workspace/marker-0.txt"), "executed").unwrap();
    assert!(checks::prepare(root, &sha).is_err());
    fs::remove_file(root.join("workspace/marker-0.txt")).unwrap();
    fs::write(root.join("workspace/existing"), "changed").unwrap();
    assert!(checks::prepare(root, &sha).is_err());
    fs::write(root.join("workspace/existing"), SENTINEL).unwrap();
    fs::write(root.join("trajectory.json"), "changed").unwrap();
    assert!(checks::prepare(root, &sha).is_err());
}

#[test]
fn result_checks_reject_tool_execution_agent_start_missing_and_duplicate_results() {
    for agent in AGENTS {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let sha = workspace(root);
        let output = root.join("output/run");
        fs::create_dir_all(output.join("native")).unwrap();
        let mut plan = manifest(agent);
        plan["source"]["sha256"] = json!(sha);
        save(&output.join("manifest.json"), &plan).unwrap();
        let name = match agent {
            "mini-swe-agent" => "prepared-prefix.json",
            "openhands" => "prepared-replay-events.json",
            "swe-agent" => "prepared-prefix.traj",
            _ => "prepared-prefix.jsonl",
        };
        let native = if name.ends_with("jsonl") {
            prefix()[0].clone()
        } else {
            prefix()
        };
        fs::write(
            output.join("native").join(name),
            serde_json::to_vec(&native).unwrap(),
        )
        .unwrap();
        let result = json!({"failure":null, "replayed_tool_calls":0, "agent_status":"not_started",
            "phase":"prepared", "quality":"verified", "continued_steps":0, "after_step":1});
        save(&output.join("result.json"), &result).unwrap();
        checks::verify(root, agent, &sha).unwrap();
        for (key, replacement) in [
            ("failure", json!({"message":"failed"})),
            ("replayed_tool_calls", json!(1)),
            ("agent_status", json!("completed")),
            ("phase", json!("continued")),
            ("quality", json!("unverified")),
            ("continued_steps", json!(1)),
            ("after_step", json!(2)),
        ] {
            let mut bad = result.clone();
            bad[key] = replacement;
            save(&output.join("result.json"), &bad).unwrap();
            assert!(checks::verify(root, agent, &sha).is_err(), "{agent} {key}");
        }
        let mut missing = result.clone();
        missing.as_object_mut().unwrap().remove("failure");
        save(&output.join("result.json"), &missing).unwrap();
        assert!(checks::verify(root, agent, &sha).is_err());
        save(&output.join("result.json"), &result).unwrap();
        fs::create_dir(root.join("output/duplicate")).unwrap();
        save(&root.join("output/duplicate/result.json"), &result).unwrap();
        assert!(checks::verify(root, agent, &sha).is_err());
    }
}

#[cfg(unix)]
#[test]
fn command_failure_timeout_and_descendants_are_supervised() {
    let mut fail = Command::new("sh");
    fail.args(["-c", "exit 7"]);
    let invocation = command_duration(fail, Duration::from_secs(2)).unwrap();
    assert_eq!(invocation.status.code(), Some(7));
    assert!(!invocation.timed_out);
    let mut hang = Command::new("sh");
    hang.args(["-c", "sleep 30 & wait"]);
    let invocation = command_duration(hang, Duration::from_millis(30)).unwrap();
    assert!(invocation.timed_out);
    assert!(!invocation.status.success());
    assert!(invocation.elapsed < Duration::from_secs(5));
    assert!(
        command_duration(
            Command::new("/nonexistent/pvisor-replay"),
            Duration::from_secs(1)
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn exited_leader_stays_waitable_until_group_cleanup_and_explicit_reap() {
    use std::os::unix::process::CommandExt;
    for exit in [0, 7, 23] {
        let mut child = Command::new("sh")
            .args(["-c", &format!("exit {exit}")])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        wait_for_exit_without_reaping(pid).unwrap();
        // WNOWAIT must be repeatable. If the first observation reaped, this
        // second observation would fail with ECHILD and the PID could recycle.
        wait_for_exit_without_reaping(pid).unwrap();
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0);
        kill_group(pid);
        assert_eq!(child.wait().unwrap().code(), Some(exit));
        assert_eq!(
            wait_for_exit_without_reaping(pid)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    // The full invocation path must retain exact ordinary statuses repeatedly,
    // rather than report the cleanup SIGKILL as the leader's result.
    for exit in [0, 7, 0, 23, 0, 7] {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("exit {exit}")]);
        let invocation = command_duration(command, Duration::from_secs(2)).unwrap();
        assert_eq!(invocation.status.code(), Some(exit));
        assert!(!invocation.timed_out);
    }
}

#[cfg(unix)]
#[test]
fn normal_exit_cleans_descendant_before_releasing_its_external_gate() {
    let temp = tempfile::tempdir().unwrap();
    let gate = temp.path().join("gate");
    let proof = temp.path().join("survived");
    let mut command = Command::new("sh");
    // The descendant cannot produce its proof until after command_duration
    // returns and the test releases the gate; no race against leader exit.
    command
        .args([
            "-c",
            "sh -c 'while [ ! -e \"$1\" ]; do sleep 0.01; done; printf survived > \"$2\"' child \"$1\" \"$2\" & exit 0",
            "leader",
        ])
        .arg(&gate)
        .arg(&proof)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let invocation = command_duration(command, Duration::from_secs(2)).unwrap();
    assert_eq!(invocation.status.code(), Some(0));
    assert!(!invocation.timed_out);
    fs::write(gate, "released").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !proof.exists(),
        "an invocation descendant survived group cleanup"
    );
}

#[cfg(unix)]
#[test]
fn failure_evidence_is_retained_and_batch_reuse_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("fake-replay");
    fs::write(
        &source,
        "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n\
         case \"$1\" in\n\
         --state-dir|--output-dir) shift; mkdir -p \"$1\"; printf partial > \"$1/partial\";;\n\
         esac\nshift\ndone\nprintf stdout\nprintf stderr >&2\nexit 7\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let output = temp.path().join("batch");
    let batch = Batch::create_at(&source, &output).unwrap();
    assert!(Batch::create_at(&source, &output).is_err());
    let mut fixture = batch.fixture("codex").unwrap();
    assert!(fixture.invoke(false).is_err());
    let trial = batch.root.join("trials/codex/00000001");
    let record = checks::read_json(&trial.join("operation.json")).unwrap();
    assert_eq!(record["correctness"], "failed");
    assert_eq!(record["exit_code"], 7);
    assert!(
        record["elapsed_ns"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap()
            > 0
    );
    assert_eq!(
        fs::read_to_string(trial.join("stdout.log")).unwrap(),
        "stdout"
    );
    assert_eq!(
        fs::read_to_string(trial.join("stderr.log")).unwrap(),
        "stderr"
    );
    for name in ["state", "output"] {
        assert_eq!(
            fs::read_to_string(trial.join(name).join("partial")).unwrap(),
            "partial"
        );
        assert!(!fixture.root.join(name).exists());
    }
}

/// A bounded seven-command smoke, not a Criterion experiment or model execution.
/// Explicit runner: PVISOR_REPLAY_TEST_BINARY=/absolute/pvisor-replay cargo nextest
/// run --manifest-path benchmark/criterion/Cargo.toml --run-ignored only
/// -E 'test(replay::tests::native_cli_all_seven)'
#[test]
#[ignore = "requires an explicitly supplied prebuilt native replay CLI"]
fn native_cli_all_seven() {
    let source = PathBuf::from(
        env::var_os("PVISOR_REPLAY_TEST_BINARY")
            .expect("set PVISOR_REPLAY_TEST_BINARY to a prebuilt native CLI"),
    )
    .canonicalize()
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let output = env::var_os("PVISOR_REPLAY_TEST_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("batch"));
    let batch = Batch::create_at(&source, &output).unwrap();
    for agent in AGENTS {
        let mut fixture = batch.fixture(agent).unwrap();
        fixture.invoke(true).unwrap();
        checks::prepare(&fixture.root, &fixture.source_sha).unwrap();
        let archived = batch.root.join("trials").join(agent).join("00000001");
        assert!(archived.join("state").is_dir());
        assert!(archived.join("output").is_dir());
        assert_eq!(
            checks::read_json(&archived.join("audit.json")).unwrap()["correctness"],
            "passed"
        );
    }
}

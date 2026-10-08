use std::{net::TcpListener, process::Command};

use pvisor::{RunBundle, RunConfig};
use pvisor_journal::api::{Journal, JournalStore};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn only_run_dir(run_home: &std::path::Path) -> std::path::PathBuf {
    let runs = std::fs::read_dir(run_home)
        .expect("list Run Home")
        .map(|entry| entry.expect("read Run Home entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("run-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected exactly one Run directory");
    runs.into_iter().next().unwrap()
}

#[test]
#[cfg(target_os = "macos")]
fn unsupported_memory_limit_preserves_observed_file_size_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let runs = temporary.path().join("runs");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .current_dir(temporary.path())
        .env("PVISOR_RUN_HOME", &runs)
        .args([
            "run",
            "--memory",
            "256MiB",
            "--max-file-size",
            "1MiB",
            "--",
            "/usr/bin/true",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run_dir(&runs)).unwrap();
    assert_eq!(bundle.resources.requested.memory_bytes, Some(268435456));
    assert_eq!(bundle.resources.effective.memory_bytes, None);
    assert_eq!(bundle.resources.effective.file_size_bytes, Some(1048576));
    assert!(
        bundle
            .resources
            .mechanisms
            .iter()
            .any(|mechanism| mechanism == "posix-rlimit")
    );
    assert!(
        bundle
            .resources
            .limitations
            .iter()
            .any(|limitation| limitation.contains("RLIMIT_AS"))
    );
    assert!(
        !bundle
            .executor_observations
            .enforcement
            .is_enforced(pvisor_core::CapabilityDimension::Resources)
    );
}

#[test]
fn safe_preset_reaches_the_run_and_reports_its_limits() {
    #[cfg(target_os = "macos")]
    if !std::path::Path::new("/Library/Filesystems/macfuse.fs").is_dir() {
        eprintln!("skipping macOS safe preset integration: macFUSE is not installed");
        return;
    }
    let temporary = tempfile::Builder::new()
        .prefix("pvsafe")
        .tempdir_in("/tmp")
        .unwrap();
    let run_home = temporary.path().join("runs");
    let stage = temporary.path().join("stage");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".ssh")).unwrap();
    std::fs::write(workspace.join(".ssh/id_ed25519"), "dummy-private-key").unwrap();
    std::fs::write(workspace.join(".env"), "warn-only-fixture").unwrap();
    std::os::unix::fs::symlink(".ssh/id_ed25519", workspace.join("alias")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--safe", "--stage"])
        .arg(&stage)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "test ! -e .ssh/id_ed25519 && ! cat .ssh/id_ed25519 && ! cat alias && cat .env",
        ])
        .current_dir(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    #[cfg(target_os = "linux")]
    if std::env::var_os("PVISOR_TEST_ALLOW_NO_USERNS").is_some()
        && !output.status.success()
        && stderr.lines().any(|line| {
            line == "Error: required sandbox unavailable: Linux rootless namespaces must be enabled"
        })
    {
        assert!(!stage.join("run-bundle.json").exists());
        eprintln!(
            "safe correctly refused to run without rootless namespaces; skipping runtime assertions on this optional shard"
        );
        return;
    }
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("CLI > safe preset > config > defaults"));
    assert!(stderr.contains("sensitive file access warning"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("dummy-private-key"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("warn-only-fixture"));
    assert!(stderr.contains("cannot distinguish inference"));
    let bundle = RunBundle::read(&stage).unwrap();
    assert!(
        bundle
            .filesystem
            .as_ref()
            .unwrap()
            .access_policy
            .deny()
            .contains(&"**/.ssh".into())
    );
    assert_eq!(bundle.network.policy["mode"], "allowlist");
    assert!(matches!(
        bundle.run.executor.unwrap().isolation,
        pvisor_core::IsolationKind::HostProcess
            | pvisor_core::IsolationKind::RootlessProcess
            | pvisor_core::IsolationKind::SandboxedProcess
    ));
}

#[test]
fn network_run_uses_the_current_workspace_and_external_run_home() {
    let temporary = tempfile::Builder::new()
        .prefix("pv")
        .tempdir_in("/tmp")
        .expect("create short temporary run root");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port");
    let listen = listener.local_addr().unwrap().to_string();
    let run_home = temporary.path().join("runs");
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--overlaynet-listen"])
        .arg(&listen)
        .args(["--overlaynet-deny-all", "--", "/usr/bin/true"])
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("execute network-only pvisor without an explicit workspace");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = only_run_dir(&run_home);

    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("run.json")).expect("read finalized Run record"),
    )
    .expect("decode finalized Run record");
    assert_eq!(record["state"], "completed");
    assert_eq!(record["command"][0], "/usr/bin/true");
    let bundle = RunBundle::read(&run_dir).expect("read generated Run Bundle");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.network.interception.is_some());
    let capture = bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "capture")
        .expect("proxy run retains its event journal");
    let journal = Journal::open(&capture.path.join("events.trace.jsonl")).unwrap();
    assert!(!journal.records().unwrap().is_empty());
}

#[test]
fn toml_and_cli_share_one_run_configuration() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let config_path = temporary.path().join("run.toml");

    let mut config = RunConfig::default();
    config.run.agent = "from-toml".into();
    config.run.command = vec!["/usr/bin/false".into()];
    std::fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--config"])
        .arg(&config_path)
        .args(["--name", "from-cli", "--", "/usr/bin/true"])
        .current_dir(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("execute pvisor from TOML plus CLI overrides");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run_dir = only_run_dir(&run_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["agent"], "from-cli");
    assert_eq!(record["command"][0], "/usr/bin/true");
    assert_eq!(record["stage_dir"], serde_json::Value::Null);

    assert_eq!(
        record["workspace"],
        workspace.canonicalize().unwrap().display().to_string()
    );
    let bundle = RunBundle::read(&run_dir).expect("read generated Run Bundle");
    assert_eq!(bundle.run.run_id, record["run_id"]);
    assert_eq!(bundle.run.agent, "from-cli");
    assert!(bundle.safety.safe_profile_requested);

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["status", "--review", "--json"])
        .arg(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("review generated Run Bundle");
    assert!(
        review.status.success(),
        "pvisor status --review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(
        reviewed["schema_version"],
        pvisor::RUN_BUNDLE_SCHEMA_VERSION
    );
    assert_eq!(reviewed["run"]["agent"], "from-cli");
}

#[test]
fn one_workspace_accepts_multiple_independent_runs() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    std::fs::create_dir(&workspace).unwrap();

    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["run", "--", "/usr/bin/true"])
            .current_dir(&workspace)
            .env("PVISOR_RUN_HOME", &run_home)
            .output()
            .expect("execute pVisor Run");
        assert!(
            output.status.success(),
            "pvisor failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let records = std::fs::read_dir(&run_home)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("run.json").is_file())
        .count();
    assert_eq!(records, 2);

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["status"])
        .current_dir(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("resolve latest Run from reusable workspace");
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["status", "--review", "last"])
        .current_dir(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("resolve last Run in reusable workspace");
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
}

#[test]
fn current_directory_selects_the_host_process_working_directory() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    std::fs::create_dir(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--", "/bin/pwd"])
        .current_dir(&workspace)
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("execute pVisor in selected workspace");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        workspace.canonicalize().unwrap().display().to_string()
    );
}

#[test]
fn record_destination_survives_toml_round_trip() {
    let mut config = RunConfig::default();
    config.record.destination = Some("/tmp/pvisor/events".into());

    let encoded = toml::to_string_pretty(&config).expect("serialize RunConfig");
    let decoded: RunConfig = toml::from_str(&encoded).expect("deserialize RunConfig");
    assert_eq!(
        decoded.record.destination.as_deref(),
        Some(std::path::Path::new("/tmp/pvisor/events"))
    );
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "temporary OCI control mount is not visible to nested pVisor on macOS"
)]
fn container_executor_runs_through_an_oci_compatible_control_surface() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let runtime = temporary.path().join("fake-oci");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
set -eu
bundle=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle) bundle="$2"; shift 2 ;;
    *) shift ;;
  esac
done
spec=$(find "$(dirname "$bundle")" -name run-spec.json -print -quit)
control=$(dirname "$spec")
exec "$PVISOR_TEST_PVISOR" run --executor host \
  --spec "$control/run-spec.json" \
  --result-file "$control/run-result.json"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--executor", "container", "--container-runtime"])
        .arg(&runtime)
        .args(["--container-pvisor-binary", env!("CARGO_BIN_EXE_pvisor")])
        .args(["--rootfs"])
        .arg(&workspace)
        .args([
            "--container-platform",
            "linux/amd64",
            "--container-network",
            "none",
            "--",
            "/bin/sh",
            "-c",
            "test \"$PVISOR_RUNTIME\" = 1 && printf container-ok",
        ])
        .current_dir(&workspace)
        .env("PVISOR_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("execute pvisor with fake OCI runtime");
    assert!(
        output.status.success(),
        "pvisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The fake runtime validates the OCI control surface; delegated stdout is
    // intentionally persisted in the nested Run Bundle rather than forwarded
    // by this transport fixture.

    let run_dir = only_run_dir(&run_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(record["executor"]["kind"], "container");
    assert_eq!(record["executor"]["isolation"], "container");
    let bundle = RunBundle::read(&run_dir).unwrap();
    assert!(!bundle.safety.host_process);
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.name.as_str()),
        Some("oci-pvisor")
    );
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "temporary OCI control mount is not visible to nested pVisor on macOS"
)]
fn container_executor_deadline_stops_the_runtime_client() {
    let temporary = tempfile::tempdir().expect("create CLI fixture");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run_home = temporary.path().join("runs");
    let runtime = temporary.path().join("fake-oci");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
set -eu
bundle=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle) bundle="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$bundle" ] || exit 0
control=$(dirname "$bundle")
exec "$PVISOR_TEST_PVISOR" run --executor host --stdio capture \
  --spec "$control/run-spec.json" \
  --result-file "$control/run-result.json"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--timeout", "20ms", "--container-runtime"])
        .arg(&runtime)
        .args(["--container-pvisor-binary", env!("CARGO_BIN_EXE_pvisor")])
        .arg("--container-rootfs")
        .arg(&workspace)
        .args([
            "--container-platform",
            "linux/amd64",
            "--container-network",
            "none",
            "--",
            "/bin/sleep",
            "30",
        ])
        .current_dir(&workspace)
        .env("PVISOR_TEST_PVISOR", env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .output()
        .expect("execute deadline-bound container Run");
    assert!(!output.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "container deadline cleanup took {:?}",
        started.elapsed()
    );

    let run_dir = only_run_dir(&run_home);
    let bundle = RunBundle::read(&run_dir).unwrap();
    assert_eq!(bundle.run.state, pvisor_core::RunState::Failed);
    let failure_kind = bundle.run.failure.as_ref().map(|failure| failure.kind);
    assert_eq!(
        failure_kind,
        Some(pvisor_core::RunFailureKind::DeadlineExceeded),
        "expected a running agent to reach its deadline: {:?}",
        bundle.run.failure
    );
    assert!(!bundle.safety.host_process);
}

#[test]
fn every_public_run_option_is_accepted_by_the_real_cli_parser() {
    let cases: &[&[&str]] = &[
        &["--config", "config.toml"],
        &["--spec", "run-spec.json"],
        &["--no-agent-defaults"],
        &["--result-file", "result.json"],
        &["--stage", "runs/task"],
        &["--stage", "drop"],
        &["--stage", "drop:/tmp/task"],
        &["--name", "smoke"],
        &["--executor", "host"],
        &["--executor", "container"],
        &["--executor", "vm"],
        &["--vm"],
        &["--rootfs", "host"],
        &["--rootfs", "/tmp/rootfs"],
        &["--rootfs", "image=/tmp/image"],
        &["--image-store", "/tmp/images"],
        &["--vm-library-dir", "/tmp/libkrunfw"],
        &["--memory", "256MiB"],
        &["--mem", "256MiB"],
        &["--cpu", "2"],
        &["--strict"],
        &["--safe"],
        &["--timeout", "1s"],
        &["--stdio", "capture"],
        &["--pass-env", "PATH"],
        &["--clear-pass-env"],
        &["--max-processes", "8"],
        &["--max-cpu-time", "5s"],
        &["--max-open-files", "32"],
        &["--max-file-size", "1MiB"],
        &["--overlayfs-max-size", "2GiB"],
        &["--container-runtime", "runc"],
        &["--container-image", "alpine:latest"],
        &["--container-rootfs", "/tmp/rootfs"],
        &["--container-pvisor-binary", "/tmp/pvisor"],
        &["--container-platform", "linux/amd64"],
        &["--container-network", "none"],
        &["--container-workdir", "/workspace"],
        &["--container-user", "1000:1000"],
        &["--container-read-only-rootfs"],
        &["--container-mount", "source=\"/tmp\",target=\"/workspace\""],
        &["--mount", "/tmp/lower:read"],
        &["--access", "**/.ssh:deny"],
        &["--overlaynet", "proxy"],
        &["--overlaynet", "auto"],
        &["--overlaynet"],
        &["--overlaynet-listen", "127.0.0.1:18080"],
        &["--overlaynet-allow", "example.com:443"],
        &["--overlaynet-deny", "10.0.0.0/8"],
        &["--overlaynet-limit", "example.com=1mbps"],
        &["--overlaynet-deny-all"],
        &["--gateway-mode", "capture"],
        &["--gateway-admin-listen", "127.0.0.1:19090"],
        &["--gateway-level", "full"],
        &["--gateway-session-header", "X-Session-ID"],
        &["--gateway-debug"],
        &[
            "--gateway-route",
            "name=\"default\",upstream=\"https://example.com\"",
        ],
        &["--record-destination", "/tmp/events.jsonl"],
    ];
    // `--help` short-circuits before execution, so this exercises the parser
    // over the whole public surface without starting a Run.
    for options in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .arg("run")
            .args(*options)
            .arg("--help")
            .output()
            .expect("run pvisor CLI parser");
        assert!(
            output.status.success(),
            "CLI rejected {:?}: {}",
            options,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn advertised_run_options() -> std::collections::BTreeSet<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--help"])
        .output()
        .expect("render pvisor run --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    let options = help
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .filter(|token| token.starts_with("--") && token.len() > 2)
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    for anchor in ["--executor", "--stage", "--strict", "--safe"] {
        assert!(
            options.contains(anchor),
            "help scraping is broken: {anchor} is missing from {options:?}"
        );
    }
    options
}

#[test]
fn removed_run_options_stay_off_the_cli_surface() {
    // `run` takes a trailing var arg that allows hyphen values, so an unknown
    // `--flag` joins the Agent command instead of failing to parse. Exit codes
    // cannot distinguish supported options from trailing command arguments;
    // the rendered option list can.
    let advertised = advertised_run_options();
    for option in [
        "--workspace",
        "--run-spec",
        "--no-config",
        "--filesystem-backend",
        "--filesystem-max-size",
        "--run-home",
        "--agent",
        "--host-rootfs",
        "--max-file-size-bytes",
        "--timeout-ms",
        "--max-cpu-time-ms",
        "--overlaynet-mode",
        "--overlayfs-base",
        "--overlayfs-target",
    ] {
        assert!(
            !advertised.contains(option),
            "`pvisor run --help` advertises the removed option {option} again"
        );
    }
}

#[test]
fn recording_uses_one_fact_journal_with_execution_phases() {
    let dir = tempfile::tempdir().unwrap();
    let recording = dir.path().join("recording");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--record-destination"])
        .arg(&recording)
        .args(["--", "/bin/sh", "-c", "exit 0"])
        .current_dir(dir.path())
        .env("PVISOR_RUN_HOME", dir.path().join("runs"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = Journal::read(&recording.join("events.trace.jsonl")).unwrap();
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event.data, pvisor_core::event::Fact::Requested { .. }))
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event.data, pvisor_core::event::Fact::Completed { .. }))
    );
    assert!(!recording.join("events.jsonl").exists());
    assert!(!recording.join("events.wal.jsonl").exists());
}

use pvisor_replay::api::{
    AgentKind, AgentProfile, OverlayFsConfig, OverlayNetConfig, PlaybackRequest,
    REQUEST_SCHEMA_VERSION, ReplayConfiguration, ReplayError, ReplayErrorClassification,
    ReplayErrorKind, ReplayMode, ReplayRequest, ReplayToml, RunConfig,
};
use std::{fs, path::Path};

#[test]
fn defaults_and_loading_preserve_paths_without_initializing_runtime_state() {
    let run = RunConfig::default();
    assert!(!run.safe && !run.inherit_env && run.pass_env.is_empty());
    assert!(run.executor.is_none() && run.timeout_ms.is_none() && run.policy.is_none());
    let overlay = OverlayFsConfig::default();
    assert!(overlay.path.is_none() && overlay.compose.is_empty() && overlay.commit.is_none());
    let network = OverlayNetConfig::default();
    assert!(network.mode.is_none() && network.policy.is_none());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(
        &path,
        "[replay]\nagent='codex'\ntrajectory='relative.jsonl'\nafter_step=1\nprepare_only=true\n",
    )
    .unwrap();
    let request = ReplayToml::from_file(&path)
        .unwrap()
        .into_request(Path::new("/chosen/workspace"))
        .unwrap();
    assert_eq!(request.workspace, Path::new("/chosen/workspace"));
    assert_eq!(request.trajectory, Path::new("relative.jsonl"));
    assert_eq!(request.mode, ReplayMode::PrepareOnly);
    assert_eq!(
        request.state_dir,
        Path::new("/tmp/pvisor-sandbox-replay/state")
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn json_validation_and_consuming_execution_refuse_before_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("request.json");
    let state = root.path().join("state");
    let output = root.path().join("output");
    let valid = serde_json::json!({
        "schema_version": REQUEST_SCHEMA_VERSION, "agent": {"type":"codex"},
        "trajectory":root.path().join("missing.jsonl"), "after_step":0,
        "workspace":root.path(), "state_dir":state, "output_dir":output,
        "boundary_user_prompt":""
    });
    for (key, value) in [
        ("schema_version", serde_json::json!("unknown")),
        ("unknown", serde_json::json!(true)),
        ("agent", serde_json::json!({"type":"unknown"})),
    ] {
        let mut bad = valid.clone();
        bad[key] = value;
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(PlaybackRequest::from_json(&path).is_err());
        assert!(!state.exists() && !output.exists());
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    let request = PlaybackRequest::from_json(&path).unwrap();
    assert_eq!(request.boundary_user_prompt.as_deref(), Some(""));
    let error = request.execute().unwrap_err();
    assert_eq!(error.kind, ReplayErrorKind::Configuration);
    assert!(error.message.contains("after_step"));
    assert!(!state.exists() && !output.exists());
}

#[test]
fn error_categories_and_diagnostic_locations_are_owned_values() {
    use ReplayErrorKind::*;
    for (kind, category, code, retryable) in [
        (Configuration, "configuration_error", 2, false),
        (Trajectory, "trajectory_error", 10, false),
        (UnsupportedAgent, "unsupported_agent", 11, false),
        (UnsupportedVersion, "unsupported_version", 11, false),
        (Workspace, "workspace_error", 20, false),
        (Executor, "executor_error", 30, false),
        (AmbiguousExecution, "ambiguous_execution", 31, false),
        (Continuation, "continuation_error", 40, false),
        (ModelRateLimit, "model_rate_limit", 40, true),
        (ModelInternal, "model_internal_error", 40, true),
        (ModelConnection, "model_connection_error", 40, true),
        (Internal, "internal_error", 50, false),
    ] {
        assert_eq!(
            (kind.category(), kind.exit_code(), kind.retryable()),
            (category, code, retryable)
        );
    }
    let mut error = ReplayError {
        kind: ReplayErrorKind::Configuration,
        message: "invalid".into(),
        locations: None,
    };
    assert!(error.locations.is_none());
    assert_eq!(error.kind.exit_code(), 2);
    error.locations = Some(pvisor_replay::api::ReplayLocations {
        run_id: "run".into(),
        state_dir: "/state".into(),
        output_dir: "/output".into(),
    });
    let locations = error.locations.as_ref().unwrap();
    assert_eq!(locations.run_id, "run");
    assert_eq!(locations.state_dir, Path::new("/state"));
    assert_eq!(locations.output_dir, Path::new("/output"));
    assert_eq!(error.to_string(), "invalid");
}

#[test]
fn agent_names_roundtrip_through_the_public_contract() {
    for agent in [
        AgentKind::ClaudeCode,
        AgentKind::Codex,
        AgentKind::MiniSweAgent,
        AgentKind::Openhands,
        AgentKind::Opencode,
        AgentKind::PiAgent,
        AgentKind::SweAgent,
    ] {
        assert_eq!(agent.as_str().parse::<AgentKind>().unwrap(), agent);
        assert!(agent.profile().contains(agent.supported_version()));
        assert_eq!(serde_json::to_value(agent).unwrap(), agent.as_str());
    }
    assert!("unknown".parse::<AgentKind>().is_err());
}

#[test]
fn toml_loading_rejects_unknown_fields_modes_and_missing_files_without_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("request.toml");
    let error = ReplayToml::from_file(&path).unwrap_err();
    assert_eq!(error.kind, ReplayErrorKind::Configuration);
    for suffix in [
        "unknown=true",
        "[run]\nunknown=true",
        "[overlayfs]\nunknown=true",
        "[overlaynet]\nunknown=true",
    ] {
        fs::write(
            &path,
            format!(
                "[replay]\nagent='codex'\ntrajectory='missing.jsonl'\nafter_step=1\n{suffix}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            ReplayToml::from_file(&path).unwrap_err().kind,
            ReplayErrorKind::Configuration
        );
    }
    for (agent, flags, kind) in [
        (
            "codex",
            "prepare_only=true\nreplay_only=true",
            ReplayErrorKind::Configuration,
        ),
        ("unknown", "", ReplayErrorKind::UnsupportedAgent),
    ] {
        fs::write(
            &path,
            format!(
                "[replay]\nagent='{agent}'\ntrajectory='missing.jsonl'\nafter_step=1\n{flags}\n"
            ),
        )
        .unwrap();
        let error = ReplayToml::from_file(&path)
            .unwrap()
            .into_request(root.path())
            .unwrap_err();
        assert_eq!(error.kind, kind);
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn json_modes_have_one_selection_and_reject_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("request.json");
    for (prepare, replay, expected) in [
        (false, false, Some(ReplayMode::ReplayAndContinue)),
        (true, false, Some(ReplayMode::PrepareOnly)),
        (false, true, Some(ReplayMode::ReplayOnly)),
        (true, true, None),
    ] {
        let value = serde_json::json!({"schema_version": REQUEST_SCHEMA_VERSION,
            "agent": {"type":"codex"}, "trajectory":"missing.jsonl", "after_step":1,
            "workspace":root.path(), "state_dir":root.path().join("state"),
            "output_dir":root.path().join("output"), "prepare_only":prepare,"replay_only":replay});
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        match expected {
            Some(mode) => assert_eq!(PlaybackRequest::from_json(&path).unwrap().mode, mode),
            None => assert_eq!(
                PlaybackRequest::from_json(&path).unwrap_err().kind,
                ReplayErrorKind::Configuration
            ),
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

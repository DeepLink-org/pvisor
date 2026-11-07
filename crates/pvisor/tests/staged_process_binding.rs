//! Staging admission through the public embedded runtime.
#![cfg(target_os = "linux")]

use pvisor::{
    NetworkDriverConfig, OverlayHint, OverlayNetMode, PVisor, ProcessExecutor, RunBundle,
};
use pvisor_core::{
    NetworkConfig, NetworkMode, RunFailureKind, RunInvocation, RunProfile, RunSpec, RunState,
};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn safe_overlay_without_workspace_binding_never_launches_write_through() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let stage = root.path().join("stage");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("state"), "original").unwrap();
    // No launcher should run: this existing executable is only needed to construct
    // the public executor. The specific Spawn diagnostic below proves rejection
    // happens in plan validation, not in the launcher or sandbox setup.
    let executor = ProcessExecutor::rootless_with_launcher("/bin/false").unwrap();
    let runtime = PVisor::builder()
        .storage(&stage)
        .instance_control(false)
        .executors(vec![Arc::new(executor)])
        .overlay(OverlayHint {
            lower_dirs: vec![project.clone()],
            stage_dir: Some(stage.clone()),
            ..Default::default()
        })
        .network(NetworkDriverConfig::new(
            OverlayNetMode::Auto,
            NetworkConfig {
                mode: NetworkMode::NoNetwork,
                ..Default::default()
            },
        ))
        .build();
    let mut spec = RunSpec::process("unbound-stage", "test", "/bin/sh");
    spec.runtime.profile = RunProfile::Safe;
    spec.runtime.timeout_ms = Some(4500);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.cwd = Some(project.display().to_string());
    process.args = vec!["-ec".into(), "printf modified > state".into()];
    let handle = tokio::time::timeout(Duration::from_secs(5), runtime.run(spec))
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.state, RunState::Failed);
    let failure = result.failure.unwrap();
    assert_eq!(failure.kind, RunFailureKind::Spawn);
    assert!(
        failure.message.contains("staged workspace requires"),
        "{failure:?}"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("state")).unwrap(),
        "original"
    );
    assert!(!stage.join("upper/state").exists());
    assert_eq!(RunBundle::read(&stage).unwrap().run.state, RunState::Failed);
}

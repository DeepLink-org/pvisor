//! Independent executor consuming only public runtime and filesystem contracts.
use async_trait::async_trait;
use pvisor::{
    ExecutionContext, ExecutorOutput, PVisor, ProcessExecutor, RunExecutor, VmExecutionInputs,
    VmStageStdio,
};
use pvisor_core::{ExecutorKind, ExecutorPlan, IsolationKind, RunInvocation, RunSpec, RunState};
use std::{path::Path, sync::Arc};

struct ExternalExecutor {
    attachments: bool,
}
#[async_trait]
impl RunExecutor for ExternalExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        let mut plan = ProcessExecutor::default().descriptor();
        plan.name = "external-contract-backend".into();
        plan.kind = ExecutorKind::VirtualMachine;
        plan.isolation = IsolationKind::VirtualMachine;
        plan.supports_checkpoint = false;
        plan
    }
    fn supports(&self, _: &RunInvocation) -> bool {
        true
    }
    fn supports_vm_network_attachment(&self) -> bool {
        self.attachments
    }
    fn supports_guest_workspace_overlay(&self) -> bool {
        self.attachments
    }
    async fn execute(&self, context: &ExecutionContext) -> ExecutorOutput {
        if self.attachments {
            use std::{
                io::{Read, Write},
                os::fd::AsRawFd,
            };
            assert_eq!(context.vm_inputs().env_unset, ["HOME"]);
            let overlay = context
                .workspace_overlay()
                .expect("runtime workspace handoff");
            assert_eq!(overlay.lowers().len(), overlay.lower_mutability().len());
            let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
                overlay.lowers().to_vec(),
                overlay.upper().to_owned(),
                overlay.work().map(Path::to_owned),
                overlay.excluded().to_vec(),
                overlay.preimages().map(Path::to_owned),
            )
            .unwrap();
            let fs = pvisor_overlay_core::service::FilesystemService::new(core)
                .with_access_policy(overlay.access_policy());
            assert!(
                fs.resolve_checked(Path::new("input.txt"))
                    .unwrap()
                    .is_some()
            );
            assert!(fs.resolve_checked(Path::new("blocked.txt")).is_err());
            let mut file = fs
                .create_file(Path::new("result.txt"), 0o600, libc::O_WRONLY)
                .unwrap();
            file.write_all(b"from external adapter").unwrap();
            drop(file);
            drop(fs);
            let config = context
                .vm_network_config()
                .unwrap()
                .expect("network addresses");
            let owner = context.take_vm_network().unwrap().expect("network owner");
            assert!(owner.is_enforcing());
            assert_eq!(owner.guest_config().address, config.address);
            assert_eq!(owner.mac_address().len(), 6);
            assert!(owner.guest_stream().as_raw_fd() >= 0);
            // Exercise the documented stream protocol with a local ARP exchange;
            // this performs no external network connection.
            let mut stream = owner.guest_stream().try_clone().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mac = owner.mac_address();
            let mut frame = vec![0xff; 6];
            frame.extend_from_slice(&mac);
            frame.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0, 6, 4, 0, 1]);
            frame.extend_from_slice(&mac);
            frame.extend_from_slice(&config.address);
            frame.extend_from_slice(&[0; 6]);
            frame.extend_from_slice(&config.gateway);
            stream
                .write_all(&(frame.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&frame).unwrap();
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let length = u32::from_be_bytes(prefix) as usize;
            assert!((42..=65535).contains(&length));
            let mut reply = vec![0; length];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply[12..14], &[0x08, 0x06]);
            assert_eq!(&reply[20..22], &[0, 2]);
            assert_eq!(&reply[28..32], &config.gateway);
            assert_eq!(&reply[38..42], &config.address);
            drop(stream);

            assert!(context.take_vm_network().unwrap().is_none());
            assert!(context.vm_network_config().unwrap().is_none());
            owner.shutdown().unwrap();
        } else {
            assert!(context.workspace_overlay().is_none());
            assert!(context.take_vm_network().unwrap().is_none());
        }
        let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
            "run_id":context.spec().run_id,"execution_id":context.execution_id(),"state":"completed",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
        })).unwrap();
        result.into()
    }
}

#[tokio::test]
async fn external_executor_installs_workspace_service_and_owns_network_teardown() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("input.txt"), "original").unwrap();
    std::fs::write(project.join("blocked.txt"), "secret").unwrap();
    let stage = root.path().join("stage");
    let overlay = pvisor::OverlayHint {
        lower_dirs: vec![project.clone()],
        stage_dir: Some(stage.clone()),
        access_policy: pvisor_core::FileAccessPolicy::new(vec!["blocked.txt".into()], vec![])
            .unwrap(),
        ..Default::default()
    };
    let runtime = PVisor::builder()
        .storage(&stage)
        .overlay(overlay)
        .instance_control(false)
        .network(pvisor::NetworkDriverConfig::new(
            pvisor::OverlayNetMode::Auto,
            pvisor_core::NetworkConfig {
                mode: pvisor_core::NetworkMode::NoNetwork,
                ..Default::default()
            },
        ))
        .executors(vec![Arc::new(ExternalExecutor { attachments: true })])
        .build();
    let mut spec = RunSpec::process("external-handoff", "test", "external-workload");
    VmExecutionInputs {
        env_unset: vec!["HOME".into()],
        guest_cwd: Some("/workspace".into()),
        stage_stdio: Some(VmStageStdio {
            interactive: false,
            tty: None,
        }),
        ..Default::default()
    }
    .apply_to(&mut spec)
    .unwrap();
    let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
    assert_eq!(result.state, RunState::Completed, "{:?}", result.failure);
    assert!(!project.join("result.txt").exists());
    assert_eq!(
        std::fs::read(stage.join("upper/result.txt")).unwrap(),
        b"from external adapter"
    );
}

#[tokio::test]
async fn attachments_are_absent_without_executor_opt_in() {
    let root = tempfile::tempdir().unwrap();
    let runtime = PVisor::builder()
        .storage(root.path())
        .instance_control(false)
        .network(pvisor::NetworkDriverConfig::new(
            pvisor::OverlayNetMode::Off,
            Default::default(),
        ))
        .executors(vec![Arc::new(ExternalExecutor { attachments: false })])
        .build();
    let result = runtime
        .run(RunSpec::process("no-handoff", "test", "external-workload"))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(result.state, RunState::Completed);
}

#[test]
fn malformed_legacy_vm_inputs_are_rejected_before_startup() {
    for (key, value) in [
        ("pvisor.vm.guest_cwd", serde_json::json!(42)),
        ("pvisor.vm.env_unset", serde_json::json!(["INVALID=NAME"])),
        (
            "pvisor.vm.stage_stdio",
            serde_json::json!({"interactive":false,"tty":[0,80]}),
        ),
        (
            "pvisor.vm.workspace_overlay",
            serde_json::json!({"upper":"/unowned"}),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("uncreated");
        let runtime = PVisor::builder().storage(&stage).build();
        let mut spec = RunSpec::process("invalid-input", "test", "/bin/true");
        spec.metadata.insert(key.into(), value);
        assert!(
            pvisor::session_service::SessionService::prepare_managed(
                &runtime,
                spec,
                Default::default()
            )
            .is_err(),
            "{key}"
        );
        assert!(!stage.exists(), "{key}");
    }
}

#[test]
fn typed_input_updates_are_transactional_and_clear_old_overrides() {
    let mut spec = RunSpec::process("typed-input", "test", "true");
    let valid = VmExecutionInputs {
        guest_cwd: Some("/work".into()),
        env_unset: vec!["HOME".into()],
        ..Default::default()
    };
    valid.apply_to(&mut spec).unwrap();
    let before = serde_json::to_value(&spec).unwrap();
    let invalid = VmExecutionInputs {
        guest_cwd: Some("relative".into()),
        ..Default::default()
    };
    assert!(invalid.apply_to(&mut spec).is_err());
    assert_eq!(before, serde_json::to_value(&spec).unwrap());
    VmExecutionInputs::default().apply_to(&mut spec).unwrap();
    assert!(!spec.metadata.contains_key("pvisor.vm.guest_cwd"));
    assert!(!spec.metadata.contains_key("pvisor.vm.env_unset"));
}

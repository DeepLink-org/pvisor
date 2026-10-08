use super::{ManagedRestoredRun, RestoredAttempt};
use crate::{OverlayHint, PVisor, RunConfig, VmExecutor};
use anyhow::Context;
use pvisor_core::{RunInvocation, RunSpec};
use std::{collections::BTreeMap, path::Path};

impl RestoredAttempt {
    /// Project captured execution inputs before handing executor configuration to
    /// the frontend. The frontend builds the visor, not the restored RunSpec.
    pub async fn start_with<F>(self, build: F) -> anyhow::Result<ManagedRestoredRun>
    where
        F: FnOnce(&RunConfig, &Path, VmExecutor, OverlayHint) -> anyhow::Result<PVisor>,
    {
        let Self {
            config,
            mut spec,
            stage,
            executor,
            overlay,
            checkpoint,
        } = self;
        let environment = executor
            .restored_guest_environment()
            .context("missing captured guest environment")?;
        project_spec(
            &mut spec,
            environment,
            &stage,
            serde_json::to_value(&checkpoint)?,
        )?;
        let visor = build(&config, &stage, executor, overlay)?;
        super::RuntimeJobService::start_managed(&visor, spec, config).await
    }
}

fn project_spec(
    spec: &mut RunSpec,
    environment: BTreeMap<String, String>,
    stage: &Path,
    checkpoint: serde_json::Value,
) -> anyhow::Result<()> {
    spec.metadata
        .remove(crate::runtime::job_execution::STORE_KEY);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.inherit_env = false;
    process.env = environment;
    spec.metadata.insert(
        "pvisor.environment".into(),
        serde_json::json!({
            "inherits_host": false, "projected_keys": process.env.keys().collect::<Vec<_>>()
        }),
    );
    spec.metadata
        .insert("pvisor.stage".into(), serde_json::to_value(stage)?);
    spec.metadata
        .insert("pvisor.orchestration.execution_restore".into(), checkpoint);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_environment_and_restore_metadata_replace_frontend_inputs() {
        let mut spec = RunSpec::process("restored-job", "sh", "/bin/sh");
        spec.metadata.insert(
            crate::runtime::job_execution::STORE_KEY.into(),
            serde_json::json!("/old/store"),
        );
        spec.metadata
            .insert("lineage-kept".into(), serde_json::json!(true));
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.inherit_env = true;
        process.env.insert("HOST_ONLY".into(), "discard".into());
        let checkpoint = serde_json::json!({"snapshot_id":"captured"});
        project_spec(
            &mut spec,
            [("CAPTURED".into(), "saved".into())].into(),
            Path::new("/restored/stage"),
            checkpoint.clone(),
        )
        .unwrap();
        let RunInvocation::Process(process) = &spec.invocation;
        assert!(!process.inherit_env);
        assert_eq!(process.env, [("CAPTURED".into(), "saved".into())].into());
        assert!(
            !spec
                .metadata
                .contains_key(crate::runtime::job_execution::STORE_KEY)
        );
        assert_eq!(spec.metadata["pvisor.stage"], "/restored/stage");
        assert_eq!(
            spec.metadata["pvisor.orchestration.execution_restore"],
            checkpoint
        );
        assert_eq!(
            spec.metadata["pvisor.environment"]["projected_keys"],
            serde_json::json!(["CAPTURED"])
        );
        assert_eq!(spec.metadata["lineage-kept"], true);
    }
}

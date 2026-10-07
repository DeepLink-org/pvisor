use super::RestoredAttempt;
use crate::runtime::{RunRecord, job_execution::Server};
use crate::{OverlayHint, PVisor, RunCancellation, RunConfig, RunHandle, VmExecutor};
use anyhow::Context;
use pvisor_core::{RunInvocation, RunResult, RunSpec};
use std::{collections::BTreeMap, future::Future, path::Path};
use tokio::task::JoinHandle;

/// Owns restored Job completion, including when a frontend stops waiting.
/// Dropping this value requests cancellation; the completion task retains the
/// Job server until native teardown and durable terminal publication finish.
#[must_use = "wait for the restored Job or explicitly cancel it"]
pub struct ManagedRestoredRun {
    handle: Option<RunHandle>,
    completion: Option<JoinHandle<anyhow::Result<RunResult>>>,
    cancellation: RunCancellation,
}

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
        let mut handle = visor.run(spec.clone()).await?;
        let cancellation = handle.cancellation();
        // No await between acceptance and installing completion ownership. On a
        // failed handoff, cancel and drain the accepted Attempt instead of merely
        // dropping its JoinHandle (which would detach a live workload).
        let server = RunRecord::read(&stage)
            .and_then(|record| Server::start(&record, config, spec, handle.controls()));
        let server = match server {
            Ok(server) => server,
            Err(error) => {
                cancellation.cancel();
                let cleanup = tokio::spawn(async move { handle.wait().await });
                return match cleanup.await {
                    Ok(Ok(_)) => Err(error.context("restored Job handoff failed; accepted Attempt drained; reconcile retained Job state")),
                    Ok(Err(wait_error)) => Err(error.context(format!("restored Job handoff failed; cleanup also failed: {wait_error}"))),
                    Err(join_error) => Err(error.context(format!("restored Job handoff failed; cleanup task also failed: {join_error}"))),
                };
            }
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // The proxy preserves RunHandle-based frontend wait adapters without
        // giving them ownership of the actual completion/publication task.
        let join = std::mem::replace(
            &mut handle.join,
            tokio::spawn(async move {
                receiver
                    .await
                    .expect("restored completion task ended without a Run result")
            }),
        );
        let completion = tokio::spawn(complete(
            join,
            move |result| async move { server.finish(&result).await },
            sender,
        ));
        Ok(ManagedRestoredRun {
            handle: Some(handle),
            completion: Some(completion),
            cancellation,
        })
    }
}

impl ManagedRestoredRun {
    /// Frontend announcement and cancellation registration happen after the Job
    /// server has taken ownership, and before invoking the wait adapter.
    pub fn handle(&self) -> &RunHandle {
        self.handle.as_ref().expect("managed wait has not started")
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn wait(self) -> anyhow::Result<RunResult> {
        self.wait_with(|handle| async move { Ok(handle.wait().await?) })
            .await
    }

    /// The callback may implement frontend signal/terminal handling. Its result
    /// is not authority to declare completion: always drain the runtime-owned
    /// task and publish the Job result, even on callback error or early return.
    pub async fn wait_with<F, Fut>(mut self, wait: F) -> anyhow::Result<RunResult>
    where
        F: FnOnce(RunHandle) -> Fut,
        Fut: Future<Output = anyhow::Result<RunResult>>,
    {
        let frontend = wait(self.handle.take().expect("managed wait starts once")).await;
        if frontend.is_err() || !self.completion.as_ref().unwrap().is_finished() {
            self.cancel();
        }
        let completed = self
            .completion
            .as_mut()
            .unwrap()
            .await
            .context("restored Job completion task failed")?;
        // Await by reference: dropping this future detaches, rather than aborts,
        // the task that owns Server::finish. Drop also requests cancellation.
        self.completion.take();
        reconcile_wait(frontend, completed)
    }
}

impl Drop for ManagedRestoredRun {
    fn drop(&mut self) {
        if self.completion.is_some() {
            self.cancellation.cancel();
        }
    }
}

async fn complete<F, Fut>(
    join: JoinHandle<RunResult>,
    publish: F,
    sender: tokio::sync::oneshot::Sender<RunResult>,
) -> anyhow::Result<RunResult>
where
    F: FnOnce(RunResult) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let result = join.await.context("restored Attempt task failed")?;
    let published = publish(result.clone()).await;
    let _ = sender.send(result.clone());
    published.context("restored Attempt ended but durable Job completion publication failed")?;
    Ok(result)
}

fn reconcile_wait(
    frontend: anyhow::Result<RunResult>,
    completed: anyhow::Result<RunResult>,
) -> anyhow::Result<RunResult> {
    match (frontend, completed) {
        (Ok(_), result) => result,
        (Err(error), Ok(_)) => Err(error),
        (Err(error), Err(completion)) => Err(error.context(format!(
            "runtime-owned restored completion also failed: {completion:#}"
        ))),
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

    fn result(code: i32) -> RunResult {
        serde_json::from_value(serde_json::json!({
            "run_id":"restored-job", "attempt_id":"restored-attempt", "state":"completed",
            "started_at_unix_ms":1, "finished_at_unix_ms":2, "exit_code":code
        }))
        .unwrap()
    }

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

    #[tokio::test]
    async fn completion_retains_publication_ownership_after_frontend_disconnect() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (published, publication) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(async { result(7) });
        let completion = tokio::spawn(complete(
            join,
            |_| async move {
                released.await.unwrap();
                published.send(()).unwrap();
                Ok(())
            },
            sender,
        ));
        drop(receiver);
        drop(completion);
        release.send(()).unwrap();
        publication.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_result_waits_for_publication_and_cannot_hide_publication_error() {
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(async { result(7) });
        let completion = tokio::spawn(complete(
            join,
            |_| async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                anyhow::bail!("publication failed")
            },
            sender,
        ));
        entering.await.unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        let frontend = receiver.await.unwrap();
        let completed = completion.await.unwrap();
        let error = reconcile_wait(Ok(frontend), completed).unwrap_err();
        assert!(format!("{error:#}").contains("publication failed"));
    }

    #[test]
    fn frontend_success_cannot_override_runtime_completion() {
        assert_eq!(
            reconcile_wait(Ok(result(0)), Ok(result(7)))
                .unwrap()
                .exit_code,
            Some(7)
        );
        assert!(
            reconcile_wait(Ok(result(0)), Err(anyhow::anyhow!("publication failed")))
                .unwrap_err()
                .to_string()
                .contains("publication failed")
        );
    }

    #[test]
    fn frontend_error_is_retained_after_cleanup() {
        assert_eq!(
            reconcile_wait(Err(anyhow::anyhow!("frontend failed")), Ok(result(0)))
                .unwrap_err()
                .to_string(),
            "frontend failed"
        );
        let error = reconcile_wait(
            Err(anyhow::anyhow!("frontend failed")),
            Err(anyhow::anyhow!("publication failed")),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("frontend failed"));
        assert!(message.contains("publication failed"));
    }
}

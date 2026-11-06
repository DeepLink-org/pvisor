use super::SessionService;
use crate::Session;
use crate::runtime::RunRecord;
use crate::{PVisor, RunConfig, RunHandle};
use anyhow::Context;
use pvisor_core::{ExecutorKind, RunResult, RunSpec};
#[cfg(test)]
use pvisor_journal::api::JournalStore;
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Owns Execution completion and durable Session publication independently of the frontend.
/// Drop requests cancellation; publication continues while the Tokio runtime lives.
#[must_use = "wait for the Session or explicitly cancel it"]
pub struct SessionRun {
    handle: Option<RunHandle>,
    completion: Option<JoinHandle<anyhow::Result<RunResult>>>,
    cancellation: CancellationToken,
}

impl SessionService {
    /// Start an Execution and install runtime-owned completion before returning it
    /// to the frontend. VM execution requires prepared durable Run storage.
    pub async fn start_managed(
        visor: &PVisor,
        spec: RunSpec,
        config: RunConfig,
    ) -> anyhow::Result<SessionRun> {
        // Resolve against the actual embedded runtime, never the supplied restoration config.
        let resolved_policy = visor.resolve_run(spec.clone())?;
        if let Some(selected) = config
            .overlayfs
            .as_ref()
            .and_then(|overlay| overlay.prepared_workspace.as_ref())
        {
            let owner = resolved_policy.preparation.workspace_base.as_ref().context(
                "prepared workspace selector requires an explicit owned runtime baseline; configuration alone does not grant ownership",
            )?;
            owner.check_selector(selected, config.vm.prepared_rootfs.as_ref())?;
        }
        let mut handle = visor.run(spec.clone()).await?;
        let cancellation = handle.execution().cancellation();
        // No await between acceptance and completion ownership installation.
        let server = (|| {
            if let Some(record) = &handle.record {
                super::policy::persist(record, &resolved_policy)?;
            }
            if handle.execution().status().execution.executor.kind != ExecutorKind::VirtualMachine {
                return Ok(None);
            }
            let prepared = handle
                .record
                .as_ref()
                .context("managed VM execution requires a durable prepared Run record")?;
            let record = RunRecord::read(&prepared.stage_dir())?;
            super::check_selected_record(prepared, &record)?;
            anyhow::ensure!(
                record.run_id == handle.run_id().as_str()
                    && record.execution_id.as_deref() == Some(handle.execution_id().as_str()),
                "managed Session handoff Execution identity mismatch"
            );
            Session::attach(&record, config, spec).map(Some)
        })();
        let server = match server {
            Ok(server) => server,
            Err(error) => {
                cancellation.cancel();
                // Start may already have published part of the Session ledger. Drain
                // independently, but do not invent a terminal publication receipt.
                let cleanup = tokio::spawn(async move { handle.wait().await });
                return match cleanup.await {
                    Ok(Ok(_)) => Err(error.context("Session handoff failed; accepted Execution drained; reconcile retained Session state")),
                    Ok(Err(wait_error)) => Err(error.context(format!("Session handoff failed; cleanup also failed: {wait_error}"))),
                    Err(join_error) => Err(error.context(format!("Session handoff failed; cleanup task also failed: {join_error}"))),
                };
            }
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // Frontend adapters receive a proxy, never the actual Execution join.
        let join = std::mem::replace(
            &mut handle.join,
            tokio::spawn(async move {
                receiver
                    .await
                    .expect("completion task ended without a Run result")
            }),
        );
        let completion = tokio::spawn(complete(
            join,
            move |result| async move {
                if let Some(server) = server {
                    server.finish(&result).await?;
                }
                Ok(())
            },
            sender,
        ));
        Ok(SessionRun {
            handle: Some(handle),
            completion: Some(completion),
            cancellation,
        })
    }
}

impl SessionRun {
    /// Frontend announcement and cancellation registration happen after the durable
    /// completion owner has taken ownership, and before invoking the wait adapter.
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
    /// task and publish the Session result, even on callback error or early return.
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
            .context("Session completion task failed")?;
        // Await by reference: dropping this future detaches, rather than aborts,
        // the task that owns Session::finish. Drop also requests cancellation.
        self.completion.take();
        reconcile_wait(frontend, completed)
    }
}

impl Drop for SessionRun {
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
    let result = join.await.context("Execution task failed")?;
    let published = publish(result.clone()).await;
    let _ = sender.send(result.clone());
    published.context("Execution ended but durable Session completion publication failed")?;
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
            "runtime-owned completion also failed: {completion:#}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::{RunRecordState, registry::RunLease};
    use crate::{ExecutionContext, ProcessExecutor, RunExecutor, Session, SessionState};
    use async_trait::async_trait;
    use pvisor_core::{ExecutorPlan, IsolationKind, RunInvocation, RunState};
    use std::{path::Path, sync::Arc};
    use tokio::sync::{oneshot, watch};

    async fn signal(mut receiver: watch::Receiver<bool>) {
        receiver.wait_for(|ready| *ready).await.unwrap();
    }

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect("managed test gate timed out")
    }

    struct Gates {
        entered: watch::Sender<bool>,
        cancelled: watch::Sender<bool>,
        release: watch::Sender<bool>,
        teardown: watch::Sender<bool>,
    }

    impl Gates {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: watch::channel(false).0,
                cancelled: watch::channel(false).0,
                release: watch::channel(false).0,
                teardown: watch::channel(false).0,
            })
        }
    }

    /// Uses the real process executor and ExecutionContext lifecycle, but advertises VM
    /// placement. It intentionally does not claim VM transport or enforcement.
    struct GatedProcess(Arc<Gates>, ExecutorKind);

    #[async_trait]
    impl RunExecutor for GatedProcess {
        fn descriptor(&self) -> ExecutorPlan {
            let mut plan = ProcessExecutor::default().descriptor();
            plan.name = "managed-test-process".into();
            plan.kind = self.1;
            if self.1 == ExecutorKind::VirtualMachine {
                plan.isolation = IsolationKind::VirtualMachine;
            }
            plan
        }

        fn supports(&self, invocation: &RunInvocation) -> bool {
            ProcessExecutor::default().supports(invocation)
        }

        async fn execute(&self, session: &ExecutionContext) -> crate::ExecutorOutput {
            self.0.entered.send_replace(true);
            let cancellation = session.execution().cancellation();
            let cancelled = tokio::select! {
                biased;
                _ = cancellation.cancelled() => true,
                _ = signal(self.0.release.subscribe()) => false,
            };
            if cancelled {
                self.0.cancelled.send_replace(true);
            }
            signal(self.0.teardown.subscribe()).await;
            if cancelled {
                let mut output: crate::ExecutorOutput = result(0).into();
                output.state = RunState::Cancelled;
                output.exit_code = None;
                output
            } else {
                ProcessExecutor::default().execute(session).await
            }
        }
    }

    fn config(kind: ExecutorKind) -> RunConfig {
        let mut config = RunConfig::default();
        config.run.executor = if kind == ExecutorKind::VirtualMachine {
            crate::config::RunExecutorKind::Vm
        } else {
            crate::config::RunExecutorKind::Host
        };
        config.overlaynet.mode = crate::OverlayNetMode::Off;
        config
    }

    fn visor(stage: &Path, gates: &Arc<Gates>, kind: ExecutorKind) -> PVisor {
        PVisor::builder()
            .storage(stage)
            .network(crate::NetworkDriverConfig::new(
                crate::OverlayNetMode::Off,
                Default::default(),
            ))
            .instance_control(false)
            .executors(vec![Arc::new(GatedProcess(gates.clone(), kind))])
            .build()
    }

    fn spec() -> RunSpec {
        let mut spec = RunSpec::process("managed-session", "test", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];
        // The caller's claimed stage is not authority for handoff.
        spec.metadata.insert(
            "pvisor.stage".into(),
            serde_json::json!("/not/the/prepared/stage"),
        );
        spec
    }

    fn observe_completion(run: &mut SessionRun) -> oneshot::Receiver<anyhow::Result<RunResult>> {
        let owner = run.completion.take().unwrap();
        let (sender, receiver) = oneshot::channel();
        // Observe the real owner without giving the frontend its join handle.
        run.completion = Some(tokio::spawn(async move {
            let completed = owner.await.unwrap();
            let forwarded = match &completed {
                Ok(result) => Ok(result.clone()),
                Err(error) => Err(anyhow::anyhow!("{error:#}")),
            };
            let _ = sender.send(forwarded);
            completed
        }));
        receiver
    }

    fn assert_terminal(stage: &Path, state: RunState, execution: bool) {
        assert_eq!(crate::RunBundle::read(stage).unwrap().run.state, state);
        let record = RunRecord::read(stage).unwrap();
        assert!(matches!(
            record.state,
            RunRecordState::Completed | RunRecordState::Cancelled
        ));
        assert!(!stage.join("execution.sock").exists());
        assert!(!stage.join("execution.sock").is_symlink());
        let _lease = RunLease::acquire(stage).expect("Execution must release the stage lease");
        if execution {
            assert_eq!(
                Session::read(&record).unwrap().unwrap().state(),
                SessionState::Terminal
            );
        } else {
            assert!(Session::read(&record).unwrap().is_none());
            assert!(!stage.join("execution-session.json").exists());
        }
    }

    #[tokio::test]
    async fn public_host_start_persists_result_without_execution_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let runtime = PVisor::builder().storage(&stage).build();
        // Even a caller-supplied VM config cannot override the actual handle kind.
        let run =
            SessionService::start_managed(&runtime, spec(), config(ExecutorKind::VirtualMachine))
                .await
                .unwrap();
        assert!(run.handle().record.is_some());
        assert!(run.handle().checkpoint_record.is_none());
        let completed = bounded(run.wait()).await.unwrap();
        assert_eq!(completed.state, RunState::Completed);
        assert_terminal(&stage, RunState::Completed, false);
    }

    #[tokio::test]
    async fn public_vm_labelled_process_publishes_terminal_and_removes_socket() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let gates = Gates::new();
        let runtime = visor(&stage, &gates, ExecutorKind::VirtualMachine);
        let run =
            SessionService::start_managed(&runtime, spec(), config(ExecutorKind::VirtualMachine))
                .await
                .unwrap();
        assert_eq!(run.handle().record.as_ref().unwrap().stage_dir(), stage);
        assert!(run.handle().checkpoint_record.is_none());
        let record = RunRecord::read(&stage).unwrap();
        let session = Session::read(&record).unwrap().unwrap();
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(
            session.active_execution,
            run.handle().execution_id().as_str()
        );
        assert!(!stage.join("execution.sock").exists());
        assert!(crate::runtime::instance_control::local_owner(&record).is_some());
        gates.release.send_replace(true);
        gates.teardown.send_replace(true);
        let completed = bounded(run.wait()).await.unwrap();
        assert_eq!(completed.state, RunState::Completed);
        assert_terminal(&stage, RunState::Completed, true);
        assert!(crate::runtime::instance_control::local_owner(&record).is_none());
    }

    #[tokio::test]
    async fn local_capture_outlives_timeout_without_creating_an_execution_socket() {
        let directory = tempfile::tempdir().unwrap();
        let stage = directory.path().join("stage");
        let gates = Gates::new();
        gates.teardown.send_replace(true);
        let runtime = visor(&stage, &gates, ExecutorKind::VirtualMachine);
        let run =
            SessionService::start_managed(&runtime, spec(), config(ExecutorKind::VirtualMachine))
                .await
                .unwrap();
        bounded(signal(gates.entered.subscribe())).await;
        let record = RunRecord::read(&stage).unwrap();
        assert!(record.live_control.is_none());
        let error = SessionService::capture_selected_execution(
            &Default::default(),
            &record,
            false,
            pvisor_core::operation::SnapshotRamStorage::Raw,
            Some("local-capture".into()),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("timed out waiting for request"));
        assert!(!stage.join("execution.sock").exists());
        let owner = crate::runtime::instance_control::local_owner(&record).unwrap();
        let permits = (0..7)
            .map(|_| owner.admit_capture().unwrap())
            .collect::<Vec<_>>();
        assert!(
            owner.admit_capture().is_err(),
            "timeout must not release an unfinished capture's admission"
        );
        drop(permits);
        // The detached request is waiting for readiness, not for the abandoned frontend.
        run.handle().execution().ready().await;
        bounded(async {
            loop {
                let session = Session::read(&record).unwrap().unwrap();
                if session
                    .requests
                    .get("local-capture")
                    .is_some_and(|request| request.error.is_some())
                {
                    assert_eq!(session.state(), SessionState::Running);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(!run.handle().execution().cancellation().is_cancelled());
        run.cancel();
        assert_eq!(
            bounded(run.wait()).await.unwrap().state,
            RunState::Cancelled
        );
    }

    #[tokio::test]
    async fn public_managed_drop_and_discarded_wait_future_still_publish() {
        for drop_wait in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let stage = temp.path().join("stage");
            let gates = Gates::new();
            let runtime = visor(&stage, &gates, ExecutorKind::VirtualMachine);
            let mut run = SessionService::start_managed(
                &runtime,
                spec(),
                config(ExecutorKind::VirtualMachine),
            )
            .await
            .unwrap();
            bounded(signal(gates.entered.subscribe())).await;
            let publication = observe_completion(&mut run);
            if drop_wait {
                let (entered, entering) = oneshot::channel();
                let mut waiting = Box::pin(run.wait_with(|_handle| async move {
                    entered.send(()).unwrap();
                    std::future::pending::<anyhow::Result<RunResult>>().await
                }));
                tokio::select! {
                    _ = &mut waiting => panic!("frontend must remain pending"),
                    _ = entering => {},
                }
                drop(waiting);
            } else {
                drop(run);
            }
            bounded(signal(gates.cancelled.subscribe())).await;
            assert_eq!(
                Session::read_stage(&stage).unwrap().unwrap().state(),
                SessionState::Running
            );
            assert!(RunLease::acquire(&stage).is_err());
            gates.teardown.send_replace(true);
            assert_eq!(
                bounded(publication).await.unwrap().unwrap().state,
                RunState::Cancelled
            );
            assert_terminal(&stage, RunState::Cancelled, true);
        }
    }

    #[tokio::test]
    async fn public_frontend_failure_early_success_and_registration_cancel_drain() {
        for mode in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let stage = temp.path().join("stage");
            let gates = Gates::new();
            let runtime = visor(&stage, &gates, ExecutorKind::VirtualMachine);
            let run = SessionService::start_managed(
                &runtime,
                spec(),
                config(ExecutorKind::VirtualMachine),
            )
            .await
            .unwrap();
            bounded(signal(gates.entered.subscribe())).await;
            let waiting = tokio::spawn(async move {
                if mode == 2 {
                    // Models cancellation registration failing after acceptance.
                    run.cancel();
                    run.wait().await
                } else {
                    run.wait_with(|_handle| async move {
                        if mode == 0 {
                            anyhow::bail!("frontend failed");
                        }
                        Ok(result(0))
                    })
                    .await
                }
            });
            bounded(signal(gates.cancelled.subscribe())).await;
            assert!(!waiting.is_finished());
            assert_eq!(
                Session::read_stage(&stage).unwrap().unwrap().state(),
                SessionState::Running
            );
            gates.teardown.send_replace(true);
            let completed = bounded(waiting).await.unwrap();
            if mode == 0 {
                assert!(
                    completed
                        .unwrap_err()
                        .to_string()
                        .contains("frontend failed")
                );
            } else {
                assert_eq!(completed.unwrap().state, RunState::Cancelled);
            }
            assert_terminal(&stage, RunState::Cancelled, true);
        }
    }

    #[tokio::test]
    async fn public_frontend_wait_cannot_hide_execution_publication_failure() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let gates = Gates::new();
        let runtime = visor(&stage, &gates, ExecutorKind::VirtualMachine);
        let run =
            SessionService::start_managed(&runtime, spec(), config(ExecutorKind::VirtualMachine))
                .await
                .unwrap();
        bounded(signal(gates.entered.subscribe())).await;
        // The Execution can still finish its own record/bundle, but Session publication
        // has lost its durable ledger. A successful proxy wait must not hide it.
        std::fs::remove_file(stage.join("execution-session.json")).unwrap();
        gates.release.send_replace(true);
        gates.teardown.send_replace(true);
        let error = bounded(run.wait()).await.unwrap_err();
        assert!(format!("{error:#}").contains("durable Session completion publication failed"));
        assert_eq!(
            crate::RunBundle::read(&stage).unwrap().run.state,
            RunState::Completed
        );
        assert!(!stage.join("execution.sock").is_symlink());
        let _lease = RunLease::acquire(&stage).unwrap();
    }

    #[tokio::test]
    async fn public_vm_start_requires_prepared_durable_storage() {
        let gates = Gates::new();
        let runtime = PVisor::builder()
            .network(crate::NetworkDriverConfig::new(
                crate::OverlayNetMode::Off,
                Default::default(),
            ))
            .instance_control(false)
            .executors(vec![Arc::new(GatedProcess(
                gates,
                ExecutorKind::VirtualMachine,
            ))])
            .build();
        let error = bounded(SessionService::start_managed(
            &runtime,
            spec(),
            config(ExecutorKind::VirtualMachine),
        ))
        .await
        .err()
        .expect("VM handoff without durable storage must fail");
        assert!(format!("{error:#}").contains("requires a durable prepared Run record"));
    }

    struct TerminalGate {
        entered: watch::Sender<bool>,
        release: watch::Sender<bool>,
    }

    struct AcceptedTerminalGate {
        entered: watch::Sender<bool>,
        release: watch::Sender<bool>,
        sink: crate::MemoryEventSink,
    }

    #[async_trait]
    impl crate::EventSink for AcceptedTerminalGate {
        async fn append(
            &self,
            event: &pvisor_core::Event,
        ) -> anyhow::Result<pvisor_core::event::Receipt> {
            let receipt = crate::EventSink::append(&self.sink, event).await?;
            if event.name() == "run.completed" {
                self.entered.send_replace(true);
                signal(self.release.subscribe()).await;
            }
            Ok(receipt)
        }
    }

    struct CompletedVm;

    #[async_trait]
    impl RunExecutor for CompletedVm {
        fn descriptor(&self) -> ExecutorPlan {
            let mut plan = ProcessExecutor::default().descriptor();
            plan.kind = ExecutorKind::VirtualMachine;
            plan.isolation = IsolationKind::VirtualMachine;
            plan
        }

        fn supports(&self, _: &RunInvocation) -> bool {
            true
        }

        async fn execute(&self, _: &ExecutionContext) -> crate::ExecutorOutput {
            result(0).into()
        }
    }

    #[tokio::test]
    async fn public_wait_abandonment_after_terminal_acceptance_preserves_completion() {
        for discard in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let stage = temp.path().join("stage");
            let sink = Arc::new(AcceptedTerminalGate {
                entered: watch::channel(false).0,
                release: watch::channel(false).0,
                sink: Default::default(),
            });
            let runtime = PVisor::builder()
                .storage(&stage)
                .network(crate::NetworkDriverConfig::new(
                    crate::OverlayNetMode::Off,
                    Default::default(),
                ))
                .instance_control(false)
                .event_sink(sink.clone())
                .executors(vec![Arc::new(CompletedVm)])
                .build();
            let mut run = SessionService::start_managed(
                &runtime,
                spec(),
                config(ExecutorKind::VirtualMachine),
            )
            .await
            .unwrap();
            let execution = run.handle().execution().clone();
            let publication = observe_completion(&mut run);
            let mut waiting = Some(Box::pin(run.wait()));
            bounded(async {
                tokio::select! {
                    _ = waiting.as_mut().unwrap() => panic!("wait must retain terminal publication"),
                    _ = signal(sink.entered.subscribe()) => {},
                }
            })
            .await;

            // The terminal audit follows executor/driver teardown and immutable
            // completion publication, but precedes managed Session completion.
            let receipt = std::fs::read(stage.join("completion.json")).unwrap();
            let bundle = std::fs::read(stage.join(crate::RUN_BUNDLE_FILENAME)).unwrap();
            assert_eq!(
                crate::RunBundle::read(&stage).unwrap().run.state,
                RunState::Completed
            );
            let record = RunRecord::read(&stage).unwrap();
            assert_eq!(
                Session::read_stage(&stage).unwrap().unwrap().state(),
                SessionState::Running
            );
            assert!(RunLease::acquire(&stage).is_err());
            assert!(crate::runtime::instance_control::local_owner(&record).is_some());

            if discard {
                waiting.take();
            } else {
                execution.cancel();
            }
            assert!(execution.cancellation().is_cancelled());
            assert!(RunLease::acquire(&stage).is_err());
            sink.release.send_replace(true);
            if let Some(waiting) = waiting {
                assert_eq!(bounded(waiting).await.unwrap().state, RunState::Completed);
            }
            let completed = bounded(publication).await.unwrap().unwrap();
            assert_eq!(completed.state, RunState::Completed);
            assert_eq!(execution.status().state, RunState::Completed);
            assert_terminal(&stage, RunState::Completed, true);
            assert!(crate::runtime::instance_control::local_owner(&record).is_none());
            assert_eq!(
                std::fs::read(stage.join("completion.json")).unwrap(),
                receipt
            );
            assert_eq!(
                std::fs::read(stage.join(crate::RUN_BUNDLE_FILENAME)).unwrap(),
                bundle
            );
            let terminals: Vec<_> = sink
                .sink
                .events()
                .into_iter()
                .filter(|event| {
                    matches!(
                        event.name(),
                        "run.completed" | "run.cancelled" | "run.failed"
                    )
                })
                .map(|event| event.name().to_owned())
                .collect();
            assert_eq!(terminals, ["run.completed"]);
        }
    }

    #[async_trait]
    impl crate::EventSink for TerminalGate {
        async fn append(
            &self,
            event: &pvisor_core::Event,
        ) -> anyhow::Result<pvisor_core::event::Receipt> {
            if event.name() == "run.cancelled" {
                self.entered.send_replace(true);
                signal(self.release.subscribe()).await;
            }
            Ok(JournalStore::append(
                &crate::trace::Journal::memory(),
                event.clone(),
            )?)
        }
    }

    #[tokio::test]
    async fn public_failed_handoff_drains_even_when_start_future_is_discarded() {
        for discard in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let stage = temp.path().join("stage");
            std::fs::create_dir(&stage).unwrap();
            // A corrupt discovery record must fail handoff without rewriting retained evidence.
            Session::new(
                "managed-session".into(),
                stage.clone(),
                Some("previous-execution".into()),
                config(ExecutorKind::VirtualMachine),
                spec(),
            )
            .unwrap()
            .write()
            .unwrap();
            let retained = std::fs::read(stage.join("execution-session.json")).unwrap();
            std::fs::create_dir(stage.join("execution-session-root.json")).unwrap();
            let sink = Arc::new(TerminalGate {
                entered: watch::channel(false).0,
                release: watch::channel(false).0,
            });
            let gates = Gates::new();
            gates.teardown.send_replace(true);
            let runtime = PVisor::builder()
                .storage(&stage)
                .network(crate::NetworkDriverConfig::new(
                    crate::OverlayNetMode::Off,
                    Default::default(),
                ))
                .instance_control(false)
                .event_sink(sink.clone())
                .executors(vec![Arc::new(GatedProcess(
                    gates,
                    ExecutorKind::VirtualMachine,
                ))])
                .build();
            let mut starting = Box::pin(SessionService::start_managed(
                &runtime,
                spec(),
                config(ExecutorKind::VirtualMachine),
            ));
            bounded(async {
                tokio::select! {
                    _ = &mut starting => panic!("handoff drain must wait for terminal gate"),
                    _ = signal(sink.entered.subscribe()) => {},
                }
            })
            .await;
            assert!(RunLease::acquire(&stage).is_err());
            assert_eq!(
                serde_json::from_slice::<Session>(
                    &std::fs::read(stage.join("execution-session.json")).unwrap()
                )
                .unwrap()
                .state(),
                SessionState::Running
            );
            if discard {
                drop(starting);
                sink.release.send_replace(true);
                // Lease release is the actual drain boundary, not a timing guess.
                bounded(async {
                    loop {
                        if let Ok(lease) = RunLease::acquire(&stage) {
                            break lease;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await;
            } else {
                sink.release.send_replace(true);
                let error = bounded(starting)
                    .await
                    .err()
                    .expect("bind conflict must fail handoff");
                assert!(format!("{error:#}").contains("accepted Execution drained"));
                let _lease = RunLease::acquire(&stage).unwrap();
            }
            assert_eq!(
                crate::RunBundle::read(&stage).unwrap().run.state,
                RunState::Cancelled
            );
            // A partially published ledger is not forged into a terminal receipt.
            assert_eq!(
                serde_json::from_slice::<Session>(
                    &std::fs::read(stage.join("execution-session.json")).unwrap()
                )
                .unwrap()
                .state(),
                SessionState::Running
            );
            assert!(stage.join("execution-session-root.json").is_dir());
            assert_eq!(
                std::fs::read(stage.join("execution-session.json")).unwrap(),
                retained
            );
        }
    }

    fn result(code: i32) -> RunResult {
        serde_json::from_value(serde_json::json!({
            "run_id":"restored-session", "execution_id":"restored-execution", "state":"completed",
            "started_at_unix_ms":1, "finished_at_unix_ms":2, "exit_code":code
        }))
        .unwrap()
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

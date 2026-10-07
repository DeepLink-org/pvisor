//! Attempt-scoped Gateway + OverlayFS session owned by pVisor.

use super::implant::{ImplantPlan, OverlayHint};
use super::overlay::{
    OverlayMount, OverlayRecord, apply_overlay, discard_overlay, hint_from_record,
    lower_stack_from_config, mount_overlay_record_observed, prepare_execution_overlay_record,
    prepare_overlay_record_mountless, resolve_overlay_workspace, stage_overlay_record,
};
use super::registry::{RunControlServer, RunLease, RunRecord, RunRecordState};
#[cfg(feature = "gateway")]
use crate::TrajectoryEventSink;
use anyhow::Context as _;
use pvisor_core::ControlController;
#[cfg(feature = "gateway")]
use pvisor_core::NetworkCapability;
use pvisor_core::{ProcessInvocation, RunInvocation, RunSpec, RunState};
#[cfg(feature = "gateway")]
use pvisor_gateway::config::ProxyConfig;
#[cfg(feature = "gateway")]
use pvisor_gateway::injection::{client_gateway_config_args, proxy_environment_with_local_auth};
#[cfg(feature = "gateway")]
use pvisor_gateway::lifecycle::{
    CaptureMode, append_lifecycle, root_session_route, session_ended_record, session_started_record,
};
#[cfg(feature = "gateway")]
use pvisor_gateway::runtime::in_process::{InProcessCapture, InProcessRuntime};
#[cfg(feature = "gateway")]
use pvisor_gateway::runtime::run_config::snapshot_proxy_config;
#[cfg(feature = "gateway")]
use pvisor_gateway::runtime::run_env::write_run_session;
#[cfg(feature = "gateway")]
use pvisor_gateway::sink::JournalObserver;
#[cfg(feature = "gateway")]
use pvisor_journal::api::JournalStore;
use pvisor_overlayfs::api::{FilesystemMetrics, FsMetrics};
use pvisor_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, InterceptionMetrics, NetworkConfig,
    NetworkPolicy,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "gateway")]
use std::time::Instant;

/// Live controls for one Attempt: capture proxy + optional overlay mount.
pub(crate) struct AttemptSession {
    proxy: Option<super::proxy::Proxy>,
    root_session: String,
    #[cfg(feature = "gateway")]
    agent_id: String,
    /// Staging record retained after unmount (for apply / discard).
    overlay_record: Option<OverlayRecord>,
    #[cfg(feature = "gateway")]
    gateway: Option<InProcessCapture>,
    vm_network: Option<Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>>,
    network_metrics: Option<InterceptionMetrics>,
    fs_metrics: Option<FsMetrics>,
    overlay: Option<OverlayMount>,
    #[cfg(feature = "gateway")]
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    #[cfg(feature = "gateway")]
    started_at: Instant,
    run_record: RunRecord,
    _control: Option<RunControlServer>,
    _lease: RunLease,
}

impl AttemptSession {
    fn finish_preparation(
        mut self,
        attempt_id: &pvisor_core::AttemptId,
        safe: bool,
        prepare: impl FnOnce(&mut Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        if let Err(error) = prepare(&mut self) {
            let snapshot = crate::AgentCtlSnapshot {
                run_id: self.run_record.run_id.clone(),
                attempt_id: attempt_id.to_string(),
                directive: pvisor_core::AgentDirective::Continue,
                clients: Vec::new(),
            };
            let message = format!("Attempt preparation failed: {error:#}");
            return match self.abort_startup(attempt_id, snapshot, safe, message) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.context(format!("startup cleanup failed: {cleanup:#}"))),
            };
        }
        Ok(self)
    }

    pub(crate) fn start_proxy(
        &mut self,
        spec: &mut RunSpec,
        network: &crate::NetworkDriverConfig,
        controller: Arc<dyn ControlController>,
        attempt_id: &pvisor_core::AttemptId,
    ) -> anyhow::Result<()> {
        let metrics = InterceptionMetrics::default();
        let proxy = super::proxy::Proxy::start(
            &network.listen,
            NetworkPolicy::compile(&network.network)?,
            controller,
            metrics.clone(),
            spec.run_id.to_string(),
            attempt_id.to_string(),
        )?;
        spec.metadata.insert(
            crate::executor::sandbox::SANDBOX_PROXY_KEY.into(),
            proxy.listen.clone().into(),
        );
        let mut plan = ImplantPlan {
            overlay: self
                .overlay_record
                .as_ref()
                .map(|record| hint_from_record(record, self.run_record.overlay_lowers.clone()))
                .unwrap_or_default(),
            ..Default::default()
        };
        let url = format!("http://{}", proxy.listen);
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            plan.env.insert(key.into(), url.clone());
        }
        for key in ["NO_PROXY", "no_proxy"] {
            plan.env
                .insert(key.into(), "localhost,127.0.0.1,::1".into());
        }
        plan.env
            .insert("PVISOR_OVERLAYNET_DRIVER".into(), "explicit-proxy".into());
        plan.env
            .insert("PVISOR_OVERLAYNET_STRENGTH".into(), "cooperative".into());
        plan.notes.push(
            "network interception: explicit proxy (cooperative; direct sockets remain ambient)"
                .into(),
        );
        super::zcode::prepare(
            spec,
            &mut plan,
            &proxy.listen,
            &self.run_record.storage,
            None,
        )?;
        let RunInvocation::Process(process) = &mut spec.invocation;
        apply_implant(process, &plan);
        super::zcode::apply_environment(process, &plan);
        self.run_record.overlaynet_listen = Some(proxy.listen.clone());
        self.run_record.network_interception =
            Some(pvisor_overlaynet::InterceptionProfile::explicit_proxy());
        self.run_record.network_policy = Some(serde_json::to_value(&network.network)?);
        self.run_record.environment.runtime_injected_keys = plan.env.keys().cloned().collect();
        self.network_metrics = Some(metrics.clone());
        self.proxy = Some(proxy);
        self.run_record.write()?;
        self._control.take();
        self._control = RunControlServer::start_observed(
            &self.run_record,
            self.fs_metrics.clone(),
            Some(metrics),
        )?;
        let metadata = spec
            .metadata
            .entry("pvisor.runtime.implant".into())
            .or_insert_with(|| plan.as_metadata_json());
        for key in ["env_keys", "notes"] {
            if let Some(values) = metadata
                .get_mut(key)
                .and_then(serde_json::Value::as_array_mut)
            {
                for value in plan.as_metadata_json()[key].as_array().unwrap() {
                    if !values.contains(value) {
                        values.push(value.clone());
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn root_session(&self) -> &str {
        &self.root_session
    }

    pub(crate) fn attachments(&self) -> crate::executor::AttemptAttachments {
        crate::executor::AttemptAttachments {
            filesystem: self
                .overlay_record
                .as_ref()
                .map(|record| record.access_policy.clone()),
            vm_network: self.vm_network.clone(),
        }
    }
    pub(crate) fn record(&self) -> RunRecord {
        self.run_record.clone()
    }

    pub(crate) fn checkpoint_record(&self) -> Option<RunRecord> {
        self.overlay_record
            .as_ref()
            .map(|_| self.run_record.clone())
    }

    pub(crate) fn execution_snapshot_store(&self) -> anyhow::Result<PathBuf> {
        super::job_execution::snapshot_store(
            &self.run_record.stage_dir(),
            &self.run_record.orchestration,
        )
    }

    pub(crate) fn teardown(
        self,
        exit_code: Option<i32>,
        executed: bool,
        hibernated: bool,
    ) -> AttemptTeardown {
        self.teardown_inner(exit_code, executed, hibernated)
    }

    fn teardown_inner(
        mut self,
        exit_code: Option<i32>,
        allow_apply: bool,
        preserve_backing: bool,
    ) -> AttemptTeardown {
        let mut errors = Vec::new();
        if let Some(proxy) = self.proxy.take()
            && let Err(error) = proxy.shutdown()
        {
            errors.push(format!("shutdown OverlayNet proxy: {error:#}"));
        }
        #[cfg(not(feature = "gateway"))]
        let _ = exit_code;
        #[cfg(feature = "gateway")]
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        #[cfg(feature = "gateway")]
        if let Some(sink) = &self.sink
            && let Err(err) = append_lifecycle(
                sink.as_ref(),
                &root_session_route(&self.root_session),
                &self.agent_id,
                session_ended_record(
                    Some(self.root_session.clone()),
                    Some(self.agent_id.clone()),
                    CaptureMode::Run,
                    "child_exit",
                    exit_code,
                    Some(duration_ms),
                ),
            )
        {
            errors.push(format!("append session.ended: {err:#}"));
        }

        let mut overlay_unmounted = true;
        let mut record = if let Some(mount) = self.overlay.take() {
            let fallback = self.overlay_record.take();
            match mount.unmount() {
                Ok(record) => Some(record),
                Err(err) => {
                    overlay_unmounted = false;
                    errors.push(format!("unmount OverlayFS: {err:#}"));
                    fallback
                }
            }
        } else {
            let mut record = self.overlay_record.take();
            if let Some(record) = record.as_mut()
                && let Err(err) = stage_overlay_record(record)
            {
                errors.push(format!("stage OverlayFS: {err:#}"));
            }
            record
        };

        if let Some(ref mut rec) = record
            && !preserve_backing
            && let Err(err) = finalize_overlay(rec, overlay_unmounted, allow_apply)
        {
            errors.push(format!("finalize OverlayFS staging: {err:#}"));
        }
        self.overlay_record = record;
        if let Some(metrics) = &self.fs_metrics {
            self.run_record.filesystem_observation = Some(metrics.snapshot());
        }

        if let Some(metrics) = &self.network_metrics {
            self.run_record.network_interception_metrics = Some(metrics.snapshot());
        }
        if let Some(network) = self.vm_network.take() {
            match network.lock() {
                Ok(mut attachment) => {
                    if let Some(attachment) = attachment.take() {
                        match attachment.shutdown() {
                            Ok(snapshot) => {
                                self.run_record.network_interception_metrics = Some(snapshot)
                            }
                            Err(err) => errors.push(format!("shutdown VM OverlayNet: {err:#}")),
                        }
                    }
                }
                Err(_) => errors.push("shutdown VM OverlayNet: attachment lock poisoned".into()),
            }
        }
        #[cfg(feature = "gateway")]
        if let Some(gateway) = self.gateway.take()
            && let Err(err) = gateway.shutdown()
        {
            errors.push(format!("shutdown Gateway: {err:#}"));
        }
        self.run_record.finished_at_unix_ms = Some(crate::util::unix_now_ms());
        self.run_record.overlay = self.overlay_record.clone();
        AttemptTeardown {
            run_record: self.run_record,
            errors,
            _lease: self._lease,
        }
    }

    /// Finalize durable state when pVisor prepared the Attempt drivers but
    /// could not hand the Attempt to an executor.
    ///
    /// Preparation writes a live RunRecord and may start an overlay, Gateway,
    /// or VM network attachment.  A later AgentCtl or event-publisher failure
    /// must therefore take the same teardown path as an executed Attempt
    /// instead of leaving a stale `running` record behind.
    pub(crate) fn abort_startup(
        self,
        attempt_id: &pvisor_core::AttemptId,
        agentctl: crate::AgentCtlSnapshot,
        safe_profile_requested: bool,
        message: String,
    ) -> anyhow::Result<()> {
        let run_id = pvisor_core::RunId::new(self.run_record.run_id.clone());
        let started_at_unix_ms = self.run_record.started_at_unix_ms;
        let mut teardown = self.teardown_inner(None, false, false);
        let mut warnings = Vec::new();
        if let Some(error) = teardown.error_message() {
            warnings.push(format!("attempt teardown after startup failure: {error}"));
        }
        let result = pvisor_core::RunResult {
            executor_observations: Default::default(),
            run_id,
            attempt_id: attempt_id.clone(),
            state: RunState::Failed,
            started_at_unix_ms,
            finished_at_unix_ms: crate::util::unix_now_ms(),
            exit_code: None,
            failure: Some(pvisor_core::RunFailure {
                kind: pvisor_core::RunFailureKind::Infrastructure,
                message,
                retryable: true,
            }),
            output: Default::default(),
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        };
        teardown.persist(&result, agentctl, safe_profile_requested)?;
        Ok(())
    }
}

// Never mutate backing data while an overlay may still be mounted. Startup
// failures retain auto-apply effects for review because execution never began.
fn finalize_overlay(
    record: &mut OverlayRecord,
    unmounted: bool,
    allow_apply: bool,
) -> anyhow::Result<()> {
    if !unmounted {
        return Ok(());
    }
    if record.auto_discard {
        discard_overlay(record)?;
    } else if record.auto_apply && allow_apply {
        apply_overlay(record)?;
    }
    Ok(())
}

pub(crate) struct AttemptTeardown {
    run_record: RunRecord,
    errors: Vec<String>,
    // Finalization is still a mutation of the Run. Keep its exclusive ownership
    // until both the durable result and terminal events have been committed.
    _lease: RunLease,
}

impl AttemptTeardown {
    pub(crate) fn run_record(&self) -> &RunRecord {
        &self.run_record
    }

    pub(crate) fn error_message(&self) -> Option<String> {
        (!self.errors.is_empty()).then(|| self.errors.join("; "))
    }

    pub(crate) fn persist(
        &mut self,
        result: &pvisor_core::RunResult,
        agentctl: crate::AgentCtlSnapshot,
        safe: bool,
    ) -> anyhow::Result<()> {
        self.commit_state(result.state)
            .context("commit local Run record failed")?;
        crate::RunBundle::capture(self.run_record(), result, agentctl, safe)?
            .write(&self.run_record().stage_dir())
            .context("write durable Run Bundle failed")?;
        Ok(())
    }

    pub(crate) fn commit_state(&mut self, state: RunState) -> anyhow::Result<()> {
        self.run_record.state = match state {
            RunState::Completed => RunRecordState::Completed,
            RunState::Cancelled => RunRecordState::Cancelled,
            RunState::Failed => RunRecordState::Failed,
            RunState::Hibernated => RunRecordState::Hibernated,
            _ => anyhow::bail!("cannot commit nonterminal Run state {state:?}"),
        };
        self.run_record.write()
    }
}

#[cfg(feature = "gateway")]
pub(crate) struct AttemptPrepareOpts<'a> {
    pub config: &'a ProxyConfig,
    /// Durable pVisor Run storage and default OverlayFS stage.
    pub storage: &'a Path,
    /// Gateway capture and session configuration storage.
    pub capture_storage: &'a Path,
    pub sink: Option<Arc<dyn TrajectoryEventSink>>,
    /// Extra overlay hint from CLI (overrides paths when set).
    pub overlay_override: OverlayHint,
    pub controller: Arc<dyn ControlController>,
    pub gateway_enabled: bool,
    pub model_wait: Option<Arc<dyn pvisor_gateway::model_wait::ModelWaitLifecycle>>,
    pub vm_network: bool,
    pub attempt_id: &'a str,
}

pub(crate) struct OverlayAttemptPrepareOpts<'a> {
    pub storage: &'a Path,
    pub overlay: OverlayHint,
    pub vm_network: Option<VmNetworkPrepareOpts>,
    pub attempt_id: &'a pvisor_core::AttemptId,
}

#[derive(Clone)]
pub(crate) struct VmNetworkPrepareOpts {
    pub network: NetworkConfig,
    pub controller: Arc<dyn ControlController>,
    pub attempt_id: String,
}

pub(crate) struct VmNetworkAttachment {
    guest_stream: std::os::unix::net::UnixStream,
    backend: pvisor_overlaynet::vm::VmNetwork,
    enforcing: bool,
}

impl VmNetworkAttachment {
    pub(crate) fn is_enforcing(&self) -> bool {
        self.enforcing
    }
    pub(crate) fn guest_stream(&self) -> &std::os::unix::net::UnixStream {
        &self.guest_stream
    }

    /// Close the peer first so a backend blocked on socket I/O can observe EOF
    /// before we join its thread.
    pub(crate) fn shutdown(self) -> anyhow::Result<pvisor_overlaynet::InterceptionSnapshot> {
        let Self {
            backend,
            guest_stream,
            enforcing: _,
        } = self;
        drop(guest_stream);
        backend.shutdown()
    }
}

fn mark_vm_network(plan: &mut ImplantPlan) {
    plan.env
        .insert("PVISOR_OVERLAYNET_DRIVER".into(), "vm-smoltcp".into());
    plan.env
        .insert("PVISOR_OVERLAYNET_STRENGTH".into(), "non-bypassable".into());
    plan.notes.push(
        "network interception: libkrun virtio-net → smoltcp (non-bypassable IPv4 TCP + DNS)".into(),
    );
}

struct PreparedVmNetwork {
    attachment: Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>,
    metrics: InterceptionMetrics,
    policy: serde_json::Value,
}

struct PreparedOverlay {
    lease: RunLease,
    mount: Option<OverlayMount>,
    fs_metrics: Option<FsMetrics>,
    hint: OverlayHint,
    record: Option<OverlayRecord>,
    lowers: Vec<std::path::PathBuf>,
}

fn initial_run_record(
    spec: &RunSpec,
    preparation: &super::run::PreparedRun,
    storage: &Path,
    attempt_id: String,
) -> anyhow::Result<RunRecord> {
    let RunInvocation::Process(process) = &spec.invocation;
    let command = std::iter::once(process.program.clone())
        .chain(process.args.iter().cloned())
        .collect::<Vec<_>>();
    Ok(RunRecord {
        attempt_id: Some(attempt_id),
        schema_version: 1,
        run_id: spec.run_id.as_str().to_string(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: spec.run_id.as_str().to_string(),
        agent: spec.agent.name.clone(),
        pid: std::process::id(),
        command,
        executor: Some((&preparation.executor).into()),
        executor_plan: Some(preparation.executor.clone()),
        state: RunRecordState::Running,
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.to_path_buf(),
        workspace: preparation.workspace.clone(),
        overlaynet_listen: None,
        network_interception: None,
        network_interception_metrics: None,
        filesystem_observation: None,
        gateway_listen: None,
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy: None,
        environment: preparation.environment.clone(),
        resource_limits: spec.runtime.resource_limits.clone(),
        overlay: None,
        overlay_lowers: Vec::new(),
        lineage: preparation.lineage.clone(),
        orchestration: orchestration_from_spec(spec),
        operation: Some(preparation.operation.clone()),
    })
}

/// Start pVisor's configured Gateway and OverlayFS drivers, then enrich `spec`.
#[cfg(feature = "gateway")]
pub(crate) fn prepare_attempt(
    spec: &mut RunSpec,
    preparation: &super::run::PreparedRun,
    opts: AttemptPrepareOpts<'_>,
) -> anyhow::Result<AttemptSession> {
    let mut config = opts.config.clone();
    spec.agent.name = config.agent_id.clone();
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());
    let capture_storage = opts
        .capture_storage
        .canonicalize()
        .unwrap_or_else(|_| opts.capture_storage.to_path_buf());

    let sink = opts.sink.unwrap_or_else(|| {
        Arc::new(JournalObserver {
            journal: crate::trace::Journal::memory(),
        }) as Arc<dyn TrajectoryEventSink>
    });
    let sink = super::supervisor::attempt_observer(sink);

    let network_metrics = InterceptionMetrics::default();
    let bandwidth_registry = BandwidthRegistry::default();
    let gateway = InProcessCapture::start_with_runtime(
        config.clone(),
        capture_storage.clone(),
        Arc::clone(&sink),
        InProcessRuntime {
            controller: Arc::clone(&opts.controller),
            interception_metrics: network_metrics.clone(),
            bandwidth_registry: bandwidth_registry.clone(),
            attempt_id: Some(opts.attempt_id.to_owned()),
            gateway_enabled: opts.gateway_enabled,
            model_wait: opts.model_wait,
        },
    )?;
    config.listen = gateway.listen.clone();
    config.admin_listen = gateway.admin_listen.clone();

    spec.metadata.insert(
        crate::executor::sandbox::SANDBOX_PROXY_KEY.into(),
        gateway.listen.clone().into(),
    );

    // A Run has one top-level identity across pVisor and Gateway.
    // Subagent capture sessions remain separate beneath this root.
    let root_session = spec.run_id.as_str().to_string();
    write_run_session(&capture_storage, &root_session)?;
    let config_snapshot = snapshot_proxy_config(&capture_storage, &root_session, &config)?;

    let mut overlay_cfg = config.overlay.clone();
    apply_overlay_override(&mut overlay_cfg, &opts.overlay_override);

    crate::util::startup_mark_run("storage.overlay_begin", spec.run_id.as_str());
    let prepared_overlay = prepare_overlay(
        &overlay_cfg,
        &storage,
        &root_session,
        preparation.guest_workspace_overlay,
        opts.overlay_override.execution_snapshot.as_ref(),
    )?;
    crate::util::startup_mark_run("storage.overlay_ready", spec.run_id.as_str());
    let PreparedOverlay {
        lease,
        mount: overlay_mount,
        fs_metrics,
        hint: overlay_hint,
        record: overlay_record,
        lowers: overlay_lowers,
    } = prepared_overlay;

    let vm_network = opts
        .vm_network
        .then(|| {
            start_vm_network(
                spec,
                VmNetworkPrepareOpts {
                    network: config.network.clone(),
                    controller: Arc::clone(&opts.controller),
                    attempt_id: opts.attempt_id.to_owned(),
                },
                Some((&gateway.listen, opts.gateway_enabled)),
                network_metrics.clone(),
                bandwidth_registry,
            )
        })
        .transpose()?;
    let mut run_record =
        initial_run_record(spec, preparation, &storage, opts.attempt_id.to_string())?;
    run_record.overlaynet_listen = Some(gateway.listen.clone());
    run_record.network_interception = Some(if opts.vm_network {
        pvisor_overlaynet::InterceptionProfile::vm_smoltcp()
    } else {
        pvisor_overlaynet::InterceptionProfile::explicit_proxy()
    });
    run_record.gateway_listen = opts.gateway_enabled.then(|| gateway.listen.clone());
    run_record.network_policy = Some(serde_json::to_value(&config.network)?);
    run_record.overlay = overlay_record.clone();
    run_record.overlay_lowers = overlay_lowers;
    let listen = gateway.listen.clone();
    let session = AttemptSession {
        proxy: None,
        root_session: root_session.clone(),
        agent_id: config.agent_id.clone(),
        overlay_record: overlay_record.clone(),
        gateway: Some(gateway),
        vm_network,
        network_metrics: Some(network_metrics),
        fs_metrics,
        overlay: overlay_mount,
        sink: Some(sink),
        #[cfg(feature = "gateway")]
        started_at: Instant::now(),
        run_record,
        _control: None,
        _lease: lease,
    };
    let safe = spec
        .metadata
        .get("pvisor.safe")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    session.finish_preparation(
        &pvisor_core::AttemptId::new(opts.attempt_id),
        safe,
        |session| {
            crate::util::startup_mark_run("storage.record_write_begin", spec.run_id.as_str());
            session.run_record.write()?;
            crate::util::startup_mark_run("storage.record_write_ready", spec.run_id.as_str());
            session._control = RunControlServer::start_observed(
                &session.run_record,
                session.fs_metrics.clone(),
                session.network_metrics.clone(),
            )?;

            let RunInvocation::Process(ref process) = spec.invocation;
            let program = process.program.clone();
            append_lifecycle(
                session.sink.as_ref().expect("Gateway observer").as_ref(),
                &root_session_route(&root_session),
                &config.agent_id,
                session_started_record(
                    Some(root_session.clone()),
                    Some(config.agent_id.clone()),
                    CaptureMode::Run,
                    Some(&listen),
                    Some(program.as_str()),
                ),
            )?;

            let implant = enrich_with_session(
                spec,
                SessionImplantOpts {
                    preparation,
                    listen: &listen,
                    root_session: &root_session,
                    overlay: &overlay_hint,
                    overlay_record: overlay_record.as_ref(),
                    run_storage: &storage,
                    capture_storage: &capture_storage,
                    config_path: &config_snapshot,
                    gateway_enabled: opts.gateway_enabled,
                    vm_network: opts.vm_network,
                    local_gateway_auth: opts.gateway_enabled
                        && config
                            .models
                            .iter()
                            .any(|route| route.api_key.is_some() || route.api_key_env.is_some()),
                },
            )?;
            session.run_record.environment.runtime_injected_keys =
                implant.env.keys().cloned().collect();
            crate::util::startup_mark_run("storage.record_write_begin", spec.run_id.as_str());
            session.run_record.write()?;
            crate::util::startup_mark_run("storage.record_write_ready", spec.run_id.as_str());
            #[cfg(unix)]
            crate::runtime::audit::arm();
            // The Attempt listener is also the VM's explicit HTTP proxy endpoint.
            // Rewrite it for every VM OverlayNet run; gateway_enabled only controls
            // LLM capture, not proxy reachability. Without this, clients in the guest
            // try to connect to their own 127.0.0.1 and fail immediately.
            if opts.vm_network {
                rewrite_vm_gateway_implant(spec, &listen);
            }
            inject_krun_overlay_metadata(spec, preparation, &overlay_hint, overlay_record.as_ref());

            Ok(())
        },
    )
}

/// Prepare a durable OverlayFS Run without enabling the optional Gateway.
pub(crate) fn prepare_overlay_attempt(
    spec: &mut RunSpec,
    preparation: &super::run::PreparedRun,
    opts: OverlayAttemptPrepareOpts<'_>,
) -> anyhow::Result<AttemptSession> {
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());
    let root_session = spec.run_id.as_str().to_string();
    let mut overlay_cfg = pvisor_core::overlay::OverlayConfig::default();
    apply_overlay_override(&mut overlay_cfg, &opts.overlay);
    crate::util::startup_mark_run("storage.overlay_begin", spec.run_id.as_str());
    let prepared_overlay = prepare_overlay(
        &overlay_cfg,
        &storage,
        &root_session,
        preparation.guest_workspace_overlay,
        opts.overlay.execution_snapshot.as_ref(),
    )?;
    crate::util::startup_mark_run("storage.overlay_ready", spec.run_id.as_str());
    let PreparedOverlay {
        lease,
        mount: overlay_mount,
        fs_metrics,
        hint: overlay_hint,
        record: overlay_record,
        lowers: overlay_lowers,
    } = prepared_overlay;
    let overlay_record = overlay_record.ok_or_else(|| {
        anyhow::anyhow!("overlay preparation requested without a target or lower directory")
    })?;

    let prepared_network = opts
        .vm_network
        .map(|network| prepare_vm_network(spec, network, None))
        .transpose()?;
    let vm_network = prepared_network
        .as_ref()
        .map(|network| Arc::clone(&network.attachment));
    let network_metrics = prepared_network
        .as_ref()
        .map(|network| network.metrics.clone());
    let network_policy = prepared_network.map(|network| network.policy);
    let mut run_record =
        initial_run_record(spec, preparation, &storage, opts.attempt_id.to_string())?;
    run_record.network_interception = vm_network
        .as_ref()
        .map(|_| pvisor_overlaynet::InterceptionProfile::vm_smoltcp());
    run_record.network_policy = network_policy;
    run_record.overlay = Some(overlay_record.clone());
    run_record.overlay_lowers = overlay_lowers;
    let session = AttemptSession {
        proxy: None,
        root_session: root_session.clone(),
        #[cfg(feature = "gateway")]
        agent_id: spec.agent.name.clone(),
        overlay_record: Some(overlay_record.clone()),
        #[cfg(feature = "gateway")]
        gateway: None,
        vm_network,
        network_metrics,
        fs_metrics,
        overlay: overlay_mount,
        #[cfg(feature = "gateway")]
        sink: None,
        #[cfg(feature = "gateway")]
        started_at: Instant::now(),
        run_record,
        _control: None,
        _lease: lease,
    };
    let safe = spec
        .metadata
        .get("pvisor.safe")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    session.finish_preparation(opts.attempt_id, safe, |session| {
        let mut plan = ImplantPlan {
            env: ImplantPlan::marker_env(),
            cwd: overlay_cwd(spec, preparation, &overlay_hint, Some(&overlay_record)),
            overlay: overlay_hint,
            notes: vec![format!(
                "filesystem: overlay target={} staging={} (apply later unless auto_apply)",
                overlay_record.target.display(),
                overlay_record.stage_dir.display()
            )],
        };
        plan.env
            .insert("PVISOR_RUN_ID".into(), spec.run_id.as_str().to_string());
        plan.env
            .insert("PVISOR_AGENT".into(), spec.agent.name.clone());
        plan.env
            .insert("PVISOR_STORAGE".into(), storage.display().to_string());
        plan.env.insert(
            "PVISOR_OVERLAY_TARGET".into(),
            overlay_record.target.display().to_string(),
        );
        plan.env.insert(
            "PVISOR_OVERLAY_STAGE".into(),
            overlay_record.stage_dir.display().to_string(),
        );
        plan.env
            .insert("PVISOR_OVERLAY_ID".into(), overlay_record.id.clone());
        if session.vm_network.is_some() {
            mark_vm_network(&mut plan);
        }
        plan.env.insert(
            "PVISOR_OVERLAY_UPPER".into(),
            overlay_record.upper.path().display().to_string(),
        );
        let RunInvocation::Process(ref mut process) = spec.invocation;
        apply_implant(process, &plan);
        session.run_record.environment.runtime_injected_keys = plan.env.keys().cloned().collect();
        crate::util::startup_mark_run("storage.record_write_begin", spec.run_id.as_str());
        session.run_record.write()?;
        crate::util::startup_mark_run("storage.record_write_ready", spec.run_id.as_str());
        session._control = RunControlServer::start_observed(
            &session.run_record,
            session.fs_metrics.clone(),
            session.network_metrics.clone(),
        )?;
        #[cfg(unix)]
        crate::runtime::audit::arm();
        spec.metadata
            .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
        inject_krun_overlay_metadata(spec, preparation, &plan.overlay, Some(&overlay_record));

        Ok(())
    })
}

/// Prepare metadata-only durable Run storage without Gateway or OverlayFS.
pub(crate) fn prepare_storage_attempt(
    spec: &mut RunSpec,
    preparation: &super::run::PreparedRun,
    storage: &Path,
    attempt_id: &pvisor_core::AttemptId,
    vm_network_opts: Option<VmNetworkPrepareOpts>,
) -> anyhow::Result<AttemptSession> {
    let storage = storage
        .canonicalize()
        .unwrap_or_else(|_| storage.to_path_buf());
    let root_session = spec.run_id.as_str().to_string();
    let lease = RunLease::acquire_new(&storage)?;
    let prepared_network = vm_network_opts
        .map(|network| prepare_vm_network(spec, network, None))
        .transpose()?;
    let vm_network = prepared_network
        .as_ref()
        .map(|network| Arc::clone(&network.attachment));
    let network_metrics = prepared_network
        .as_ref()
        .map(|network| network.metrics.clone());
    let network_policy = prepared_network.map(|network| network.policy);
    let mut run_record = initial_run_record(spec, preparation, &storage, attempt_id.to_string())?;
    run_record.network_interception = vm_network
        .as_ref()
        .map(|_| pvisor_overlaynet::InterceptionProfile::vm_smoltcp());
    run_record.network_policy = network_policy;
    let session = AttemptSession {
        proxy: None,
        root_session: root_session.clone(),
        #[cfg(feature = "gateway")]
        agent_id: spec.agent.name.clone(),
        overlay_record: None,
        #[cfg(feature = "gateway")]
        gateway: None,
        vm_network,
        network_metrics,
        fs_metrics: None,
        overlay: None,
        #[cfg(feature = "gateway")]
        sink: None,
        #[cfg(feature = "gateway")]
        started_at: Instant::now(),
        run_record,
        _control: None,
        _lease: lease,
    };
    let safe = spec
        .metadata
        .get("pvisor.safe")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    session.finish_preparation(attempt_id, safe, |session| {
        let mut plan = ImplantPlan {
            env: ImplantPlan::marker_env(),
            cwd: None,
            overlay: OverlayHint::default(),
            notes: vec![format!("durable Run storage: {}", storage.display())],
        };
        plan.env
            .insert("PVISOR_RUN_ID".into(), root_session.clone());
        plan.env
            .insert("PVISOR_AGENT".into(), spec.agent.name.clone());
        plan.env
            .insert("PVISOR_STORAGE".into(), storage.display().to_string());
        if session.vm_network.is_some() {
            mark_vm_network(&mut plan);
        }
        let RunInvocation::Process(ref mut process) = spec.invocation;
        apply_implant(process, &plan);
        session.run_record.environment.runtime_injected_keys = plan.env.keys().cloned().collect();
        crate::util::startup_mark_run("storage.record_write_begin", spec.run_id.as_str());
        session.run_record.write()?;
        crate::util::startup_mark_run("storage.record_write_ready", spec.run_id.as_str());
        session._control = RunControlServer::start(&session.run_record)?;
        #[cfg(unix)]
        crate::runtime::audit::arm();
        spec.metadata
            .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());

        Ok(())
    })
}

fn start_vm_network(
    spec: &mut RunSpec,
    opts: VmNetworkPrepareOpts,
    gateway: Option<(&str, bool)>,
    metrics: InterceptionMetrics,
    bandwidth_registry: BandwidthRegistry,
) -> anyhow::Result<Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>> {
    let policy = NetworkPolicy::compile(&opts.network)?;
    let egress =
        EgressRuntime::with_bandwidth_registry(policy, opts.controller, bandwidth_registry);
    let mut config = pvisor_overlaynet::vm::VmNetworkConfig::new(
        egress,
        EgressContext {
            run_id: Some(spec.run_id.as_str().to_owned()),
            attempt_id: Some(opts.attempt_id),
        },
    );
    config.metrics = metrics;
    if let Some((listen, _)) = gateway {
        let host: std::net::SocketAddr = listen
            .strip_prefix("http://")
            .or_else(|| listen.strip_prefix("https://"))
            .unwrap_or(listen)
            .parse()
            .with_context(|| format!("parse Attempt Gateway listen address `{listen}`"))?;
        config.gateway = Some(pvisor_overlaynet::vm::VmGatewayRoute {
            guest_port: host.port(),
            host,
        });
    }
    let (backend, guest_stream) = pvisor_overlaynet::vm::VmNetwork::start(config)?;
    spec.metadata.insert(
        "pvisor.network.driver".into(),
        serde_json::Value::String("vm-smoltcp".into()),
    );
    spec.metadata.insert(
        "pvisor.network.guest_ipv4".into(),
        serde_json::Value::String(pvisor_overlaynet::vm::GUEST_IPV4.to_string()),
    );
    Ok(Arc::new(std::sync::Mutex::new(Some(VmNetworkAttachment {
        guest_stream,
        backend,
        // Even a public policy is enforced at the guest's only network device:
        // the guest cannot bypass smoltcp by opening a host socket.
        enforcing: true,
    }))))
}

fn prepare_vm_network(
    spec: &mut RunSpec,
    opts: VmNetworkPrepareOpts,
    gateway: Option<(&str, bool)>,
) -> anyhow::Result<PreparedVmNetwork> {
    let metrics = InterceptionMetrics::default();
    let policy = serde_json::to_value(&opts.network)?;
    let attachment = start_vm_network(
        spec,
        opts,
        gateway,
        metrics.clone(),
        BandwidthRegistry::default(),
    )?;
    Ok(PreparedVmNetwork {
        attachment,
        metrics,
        policy,
    })
}

#[cfg(any(feature = "gateway", test))]
fn rewrite_vm_gateway_implant(spec: &mut RunSpec, listen: &str) {
    let listen = listen.trim_end_matches('/');
    let listen_authority = listen
        .strip_prefix("http://")
        .or_else(|| listen.strip_prefix("https://"))
        .unwrap_or(listen);
    let gateway_port = listen_authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .expect("Gateway listen address was validated before implant rewriting");
    let virtual_base = format!(
        "http://{}:{gateway_port}",
        pvisor_overlaynet::vm::ROUTER_IPV4
    );
    let mut source_bases = vec![
        format!("http://{listen_authority}"),
        format!("https://{listen_authority}"),
    ];
    if listen_authority.starts_with("127.0.0.1:") {
        source_bases.push(format!("http://localhost:{gateway_port}"));
        source_bases.push(format!("https://localhost:{gateway_port}"));
    } else if listen_authority.starts_with("localhost:") {
        source_bases.push(format!("http://127.0.0.1:{gateway_port}"));
        source_bases.push(format!("https://127.0.0.1:{gateway_port}"));
    }
    let RunInvocation::Process(process) = &mut spec.invocation;
    for value in process.env.values_mut() {
        for source in &source_bases {
            if value.contains(source) {
                *value = value.replace(source, &virtual_base);
            }
        }
    }
    for argument in &mut process.args {
        for source in &source_bases {
            if argument.contains(source) {
                *argument = argument.replace(source, &virtual_base);
            }
        }
    }
    let no_proxy = format!("127.0.0.1,localhost,{}", pvisor_overlaynet::vm::ROUTER_IPV4);
    process.env.insert("NO_PROXY".into(), no_proxy.clone());
    process.env.insert("no_proxy".into(), no_proxy);
    process
        .env
        .insert("PVISOR_GATEWAY_VIRTUAL_ADDR".into(), virtual_base);
}

fn orchestration_from_spec(
    spec: &RunSpec,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    spec.metadata
        .iter()
        .filter(|(key, _)| {
            key.starts_with("pvisor.orchestration.") || key.starts_with("pvisor.supervisor.")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn apply_overlay_override(
    overlay_cfg: &mut pvisor_core::overlay::OverlayConfig,
    overlay_override: &OverlayHint,
) {
    if let Some(durability) = overlay_override.durability {
        overlay_cfg.durability = durability;
    }
    if overlay_override != &OverlayHint::default() {
        overlay_cfg.access_policy = overlay_override.access_policy.clone();
    }
    overlay_cfg.auto_apply = overlay_override.auto_apply;
    overlay_cfg.auto_discard = overlay_override.auto_discard;
    overlay_cfg.protect_target = overlay_override.protect_target;
    if let Some(stage) = &overlay_override.stage_dir {
        overlay_cfg.stage_dir = Some(stage.display().to_string());
        overlay_cfg.enabled = true;
    }
    if let Some(merged) = &overlay_override.merged_dir {
        overlay_cfg.merged_dir = Some(merged.display().to_string());
        overlay_cfg.enabled = true;
    }
    if let Some(upper) = &overlay_override.upper_dir {
        overlay_cfg.upper_dir = Some(upper.display().to_string());
    }
    if let Some(work) = &overlay_override.work_dir {
        overlay_cfg.work_dir = Some(work.display().to_string());
    }
    if !overlay_override.lower_dirs.is_empty() {
        // The final lower is the base/apply target; preceding entries are
        // read-only compose layers ordered from highest to lowest priority.
        if overlay_cfg.target.is_none() {
            let (target, compose) = overlay_override
                .lower_dirs
                .split_last()
                .expect("non-empty lower stack");
            overlay_cfg.target = Some(target.display().to_string());
            overlay_cfg.lower_dirs = compose.iter().map(|p| p.display().to_string()).collect();
        } else {
            overlay_cfg.lower_dirs = overlay_override
                .lower_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect();
        }
        overlay_cfg.enabled = true;
    }
    if let Some(saved) = &overlay_override.execution_snapshot {
        overlay_cfg.target = Some(saved.target.display().to_string());
        overlay_cfg.lower_dirs = overlay_override
            .lower_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect();
    }
}

fn prepare_overlay(
    overlay_cfg: &pvisor_core::overlay::OverlayConfig,
    storage: &Path,
    root_session: &str,
    mountless: bool,
    execution_snapshot: Option<&super::implant::ExecutionOverlayHint>,
) -> anyhow::Result<PreparedOverlay> {
    if !overlay_cfg.enabled && overlay_cfg.target.is_none() {
        return Ok(PreparedOverlay {
            lease: RunLease::acquire_new(storage)?,
            mount: None,
            fs_metrics: None,
            hint: OverlayHint::default(),
            record: None,
            lowers: Vec::new(),
        });
    }
    match resolve_overlay_workspace(overlay_cfg, storage, root_session)? {
        Some(mut record) => {
            let lease =
                crate::util::persistence_step(root_session, "overlay", "lease_prepare", || {
                    RunLease::acquire_new(&record.stage_dir)
                })?;
            let lowers = if let Some(saved) = execution_snapshot {
                anyhow::ensure!(mountless, "execution snapshot backing requires a native VM");
                record.target = saved.target.clone();
                record.baseline_lower = saved.baseline_lower.clone();
                record.excluded_paths = saved.excluded_paths.clone();
                overlay_cfg
                    .lower_dirs
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>()
            } else {
                lower_stack_from_config(overlay_cfg, storage, &mut record, !mountless)?
            };
            let (mount, record, fs_metrics) = if mountless {
                (
                    None,
                    if execution_snapshot.is_some() {
                        prepare_execution_overlay_record(
                            &record,
                            &lowers,
                            root_session,
                            overlay_cfg.durability,
                        )?
                    } else {
                        prepare_overlay_record_mountless(
                            &record,
                            &lowers,
                            root_session,
                            overlay_cfg.durability,
                        )?
                    },
                    None,
                )
            } else {
                let metrics = FsMetrics::default();
                let mount = mount_overlay_record_observed(
                    &record,
                    &lowers,
                    Some(metrics.clone()),
                    overlay_cfg.durability,
                )?;
                let record = mount.record().clone();
                (Some(mount), record, Some(metrics))
            };
            let mut hint = hint_from_record(&record, lowers.clone());
            if mountless {
                hint.merged_dir = None;
            }
            Ok(PreparedOverlay {
                lease,
                mount,
                fs_metrics,
                hint,
                record: Some(record),
                lowers,
            })
        }
        None => Ok(PreparedOverlay {
            lease: RunLease::acquire_new(storage)?,
            mount: None,
            fs_metrics: None,
            hint: OverlayHint::default(),
            record: None,
            lowers: Vec::new(),
        }),
    }
}

fn inject_krun_overlay_metadata(
    spec: &mut RunSpec,
    preparation: &super::run::PreparedRun,
    hint: &OverlayHint,
    record: Option<&OverlayRecord>,
) {
    if !preparation.guest_workspace_overlay {
        return;
    }
    let Some(record) = record else {
        return;
    };
    let upper = &record.upper.upper_dir;
    let work = &record.upper.work_dir;
    spec.metadata.insert(
        "pvisor.vm.workspace_overlay".into(),
        serde_json::json!({
            "lowers": hint.lower_dirs,
            "apply_target": record.target,
            "baseline_lower": record.baseline_lower,
            "upper": upper,
            "work": work,
            "preimages": record.stage_dir.join("preimages"),
            "excluded": record.excluded_paths,
            "access_policy": record.access_policy,
        }),
    );
}

#[cfg(feature = "gateway")]
struct SessionImplantOpts<'a> {
    preparation: &'a super::run::PreparedRun,
    listen: &'a str,
    root_session: &'a str,
    overlay: &'a OverlayHint,
    overlay_record: Option<&'a OverlayRecord>,
    run_storage: &'a Path,
    capture_storage: &'a Path,
    config_path: &'a Path,
    gateway_enabled: bool,
    vm_network: bool,
    local_gateway_auth: bool,
}

#[cfg(feature = "gateway")]
fn enrich_with_session(
    spec: &mut RunSpec,
    opts: SessionImplantOpts<'_>,
) -> anyhow::Result<ImplantPlan> {
    let SessionImplantOpts {
        preparation,
        listen,
        root_session,
        overlay,
        overlay_record,
        run_storage,
        capture_storage,
        config_path,
        gateway_enabled,
        vm_network,
        local_gateway_auth,
    } = opts;
    let mut plan = ImplantPlan {
        env: ImplantPlan::marker_env(),
        cwd: overlay_cwd(spec, preparation, overlay, overlay_record),
        overlay: overlay.clone(),
        notes: Vec::new(),
    };

    plan.env
        .insert("PVISOR_RUN_ID".into(), spec.run_id.as_str().to_string());
    plan.env
        .insert("PVISOR_AGENT".into(), spec.agent.name.clone());
    plan.env.insert(
        "PVISOR_CAPTURE_CONFIG".into(),
        config_path.display().to_string(),
    );
    plan.env.insert(
        "PVISOR_CAPTURE_STORAGE".into(),
        capture_storage.display().to_string(),
    );
    plan.env
        .insert("PVISOR_STORAGE".into(), run_storage.display().to_string());
    plan.notes
        .push("network service: in-process HTTP proxy started".into());

    for (key, value) in proxy_environment_with_local_auth(listen, root_session, local_gateway_auth)
    {
        plan.env.insert(key, value);
    }
    if !gateway_enabled {
        for key in [
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "AZURE_OPENAI_ENDPOINT",
            "ANTHROPIC_BASE_URL",
            "GEMINI_API_BASE",
        ] {
            plan.env.remove(key);
        }
    }
    plan.notes
        .push(format!("network service: proxy env → http://{listen}"));
    if vm_network {
        mark_vm_network(&mut plan);
    } else {
        plan.env
            .insert("PVISOR_OVERLAYNET_DRIVER".into(), "explicit-proxy".into());
        plan.env
            .insert("PVISOR_OVERLAYNET_STRENGTH".into(), "cooperative".into());
        plan.notes.push(
            "network interception: explicit proxy (cooperative; direct sockets remain ambient)"
                .into(),
        );
    }

    match &spec.capabilities.network {
        NetworkCapability::Scoped { .. } => {
            plan.env
                .insert("PVISOR_NETWORK_POLICY".into(), "scoped".into());
            plan.notes
                .push("network: session > workspace > user policy".into());
        }

        NetworkCapability::Ambient => {
            plan.env
                .insert("PVISOR_NETWORK_POLICY".into(), "ambient".into());
            plan.notes
                .push("network: ambient (from capture config)".into());
        }
        NetworkCapability::Deny => {
            plan.env
                .insert("PVISOR_NETWORK_POLICY".into(), "deny".into());
            plan.notes.push(if vm_network {
                "network: deny on the non-bypassable VM data plane".into()
            } else {
                "network: deny for traffic intercepted by the proxy".into()
            });
        }
        NetworkCapability::AllowList { hosts, rules } => {
            plan.env
                .insert("PVISOR_NETWORK_POLICY".into(), "allowlist".into());
            plan.env
                .insert("PVISOR_NETWORK_ALLOWLIST".into(), hosts.join(","));
            if let Ok(serialized) = serde_json::to_string(rules) {
                plan.env.insert("PVISOR_NETWORK_RULES".into(), serialized);
            }
            plan.notes.push(format!(
                "network: allowlist ({} legacy hosts, {} structured rules, applied to {} traffic)",
                hosts.len(),
                rules.len(),
                if vm_network {
                    "VM"
                } else {
                    "intercepted proxy"
                },
            ));
        }
        NetworkCapability::Policy {
            default_action,
            allow,
            deny,
            limits,
        } => {
            plan.env.insert(
                "PVISOR_NETWORK_POLICY".into(),
                match default_action {
                    pvisor_core::NetworkDefaultAction::Allow => "default-allow",
                    pvisor_core::NetworkDefaultAction::Deny => "default-deny",
                }
                .into(),
            );
            for (key, value) in [
                ("PVISOR_NETWORK_RULES", allow),
                ("PVISOR_NETWORK_DENY", deny),
            ] {
                if let Ok(serialized) = serde_json::to_string(value) {
                    plan.env.insert(key.into(), serialized);
                }
            }
            if let Ok(serialized) = serde_json::to_string(limits) {
                plan.env.insert("PVISOR_NETWORK_LIMITS".into(), serialized);
            }
            plan.notes.push(format!(
                "network: policy ({} allow, {} deny, {} bandwidth limits, applied to {} traffic)",
                allow.len(),
                deny.len(),
                limits.len(),
                if vm_network {
                    "VM"
                } else {
                    "intercepted proxy"
                },
            ));
        }
    }

    if let Some(rec) = overlay_record {
        plan.env.insert(
            "PVISOR_OVERLAY_TARGET".into(),
            rec.target.display().to_string(),
        );
        plan.env.insert(
            "PVISOR_OVERLAY_UPPER".into(),
            rec.upper.path().display().to_string(),
        );
        plan.env.insert(
            "PVISOR_OVERLAY_STAGE".into(),
            rec.stage_dir.display().to_string(),
        );
        plan.env.insert("PVISOR_OVERLAY_ID".into(), rec.id.clone());
        plan.notes.push(format!(
            "filesystem: overlay target={} staging={} (apply later unless auto_apply)",
            rec.target.display(),
            rec.stage_dir.display()
        ));
    } else if overlay.merged_dir.is_some() {
        plan.notes
            .push("filesystem: embedded overlay merged root as cwd".into());
    } else {
        plan.notes.push("filesystem: host view (no overlay)".into());
    }

    let profile = spec
        .metadata
        .get("pvisor.gateway.profile")
        .cloned()
        .map(serde_json::from_value::<crate::config::GatewayProfile>)
        .transpose()?;
    super::zcode::prepare(spec, &mut plan, listen, run_storage, profile)?;

    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    super::zcode::apply_environment(process, &plan);
    if gateway_enabled {
        inject_gateway_args(process, listen);
    }
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
    Ok(plan)
}

fn process_cwd(spec: &RunSpec) -> Option<PathBuf> {
    match &spec.invocation {
        RunInvocation::Process(process) => process
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok()),
    }
}

fn overlay_cwd(
    spec: &RunSpec,
    preparation: &super::run::PreparedRun,
    overlay: &OverlayHint,
    record: Option<&OverlayRecord>,
) -> Option<PathBuf> {
    // Only Linux binds the merged view over the original path in a private root.
    if cfg!(target_os = "linux")
        && crate::executor::sandbox::sandbox_required(spec)
        && !preparation.guest_workspace_overlay
        && overlay.merged_dir.is_some()
        && let Some(record) = record
    {
        return Some(record.target.clone());
    }
    overlay.merged_dir.clone().or_else(|| process_cwd(spec))
}

#[cfg(feature = "gateway")]
fn inject_gateway_args(process: &mut ProcessInvocation, listen: &str) {
    let extra = client_gateway_config_args(&process.program, listen);
    if extra.is_empty() {
        return;
    }
    let mut args = extra;
    args.append(&mut process.args);
    process.args = args;
}

pub(crate) fn apply_implant(process: &mut ProcessInvocation, plan: &ImplantPlan) {
    for (key, value) in &plan.env {
        process
            .env
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if process.cwd.is_none()
        && let Some(cwd) = &plan.cwd
    {
        process.cwd = Some(cwd.display().to_string());
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn hibernation_retains_partial_workspace_without_auto_apply_or_discard() {
        for discard in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let target = temporary.path().join("target");
            let storage = temporary.path().join("storage");
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("value"), b"original").unwrap();
            let mut spec = pvisor_core::RunSpec::process("hibernated", "agent", "true");
            let preparation = crate::PVisor::new()
                .resolve_run(spec.clone())
                .unwrap()
                .preparation;
            let attempt = pvisor_core::AttemptId::new("hibernated-attempt");
            let mut session =
                super::prepare_storage_attempt(&mut spec, &preparation, &storage, &attempt, None)
                    .unwrap();
            let config = pvisor_core::overlay::OverlayConfig {
                enabled: true,
                target: Some(target.display().to_string()),
                stage_dir: Some(storage.display().to_string()),
                ..Default::default()
            };
            let record = super::resolve_overlay_workspace(&config, &storage, "hibernated")
                .unwrap()
                .unwrap();
            let mut record = super::prepare_overlay_record_mountless(
                &record,
                std::slice::from_ref(&target),
                "hibernated",
                Default::default(),
            )
            .unwrap();
            std::fs::write(record.upper.path().join("value"), b"partial").unwrap();
            record.auto_apply = !discard;
            record.auto_discard = discard;
            session.overlay_record = Some(record);
            let mut teardown = session.teardown(None, true, true);
            assert!(teardown.error_message().is_none());
            teardown
                .commit_state(pvisor_core::RunState::Hibernated)
                .unwrap();
            let record = crate::RunRecord::read(&storage).unwrap();
            assert_eq!(record.state, crate::RunRecordState::Hibernated);
            assert_eq!(
                record.overlay.as_ref().unwrap().state,
                pvisor_core::overlay::OverlayState::Staged
            );
            assert_eq!(std::fs::read(target.join("value")).unwrap(), b"original");
            assert_eq!(
                std::fs::read(record.overlay.unwrap().upper.path().join("value")).unwrap(),
                b"partial"
            );
        }
    }

    #[test]
    fn finalization_retains_backing_on_unmount_or_startup_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let stage = temporary.path().join("stage");
        let config = pvisor_core::overlay::OverlayConfig {
            enabled: true,
            target: Some(target.display().to_string()),
            stage_dir: Some(stage.display().to_string()),
            ..Default::default()
        };
        let record = super::resolve_overlay_workspace(&config, temporary.path(), "run-1")
            .unwrap()
            .unwrap();
        let mut record = super::prepare_overlay_record_mountless(
            &record,
            std::slice::from_ref(&target),
            "test-run",
            Default::default(),
        )
        .unwrap();
        std::fs::write(record.upper.path().join("value"), b"staged").unwrap();
        record.auto_discard = true;
        super::finalize_overlay(&mut record, false, true).unwrap();
        assert!(record.upper.path().join("value").exists());
        record.auto_discard = false;
        record.auto_apply = true;
        super::finalize_overlay(&mut record, true, false).unwrap();
        assert!(!target.join("value").exists());
        assert!(record.upper.path().join("value").exists());
        record.auto_discard = true;
        super::finalize_overlay(&mut record, true, false).unwrap();
        assert!(!record.upper.path().exists());
    }

    use super::rewrite_vm_gateway_implant;
    use pvisor_core::{RunInvocation, RunSpec};

    #[test]
    fn teardown_holds_the_lease_and_preparation_errors_commit_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("owned-run", "agent", "true");
        let preparation = crate::PVisor::new()
            .resolve_run(spec.clone())
            .unwrap()
            .preparation;
        let attempt_id = pvisor_core::AttemptId::new("attempt-owned");
        let session = super::prepare_storage_attempt(
            &mut spec,
            &preparation,
            temporary.path(),
            &attempt_id,
            None,
        )
        .unwrap();
        let mut teardown = session.teardown(None, false, false);
        assert!(
            teardown
                .commit_state(pvisor_core::RunState::Running)
                .is_err()
        );
        assert!(crate::runtime::is_live(temporary.path()).unwrap());
        teardown
            .commit_state(pvisor_core::RunState::Failed)
            .unwrap();
        assert!(crate::runtime::is_live(temporary.path()).unwrap());
        drop(teardown);
        assert!(!crate::runtime::is_live(temporary.path()).unwrap());

        let failed_storage = temporary.path().join("failed");
        let session = super::prepare_storage_attempt(
            &mut spec,
            &preparation,
            &failed_storage,
            &attempt_id,
            None,
        )
        .unwrap();
        assert!(
            session
                .finish_preparation(&attempt_id, false, |_| {
                    anyhow::bail!("injected preparation failure")
                })
                .is_err()
        );
        let record = crate::RunRecord::read(&failed_storage).unwrap();
        assert_eq!(record.state, crate::RunRecordState::Failed);
        assert!(record.finished_at_unix_ms.is_some());
        assert_eq!(
            crate::RunBundle::read(&failed_storage).unwrap().run.state,
            pvisor_core::RunState::Failed
        );
        assert!(!crate::runtime::is_live(&failed_storage).unwrap());
    }

    #[test]
    fn safe_overlay_cwd_uses_original_path_only_on_linux() {
        let mut spec = RunSpec::process("run-1", "agent", "sh");
        let preparation = crate::PVisor::new()
            .resolve_run(spec.clone())
            .unwrap()
            .preparation;
        spec.metadata.insert(
            crate::executor::sandbox::REQUIRED_SANDBOX_KEY.into(),
            true.into(),
        );
        let config = pvisor_core::overlay::OverlayConfig {
            enabled: true,
            target: Some("/workspace".into()),
            stage_dir: Some("/stage".into()),
            ..Default::default()
        };
        let record = super::super::overlay::resolve_overlay_workspace(
            &config,
            std::path::Path::new("/runs"),
            "run-1",
        )
        .unwrap()
        .unwrap();
        let overlay = super::OverlayHint {
            merged_dir: Some(record.merged_dir.clone()),
            ..Default::default()
        };
        let expected = if cfg!(target_os = "linux") {
            &record.target
        } else {
            &record.merged_dir
        };
        assert_eq!(
            super::overlay_cwd(&spec, &preparation, &overlay, Some(&record)).as_ref(),
            Some(expected)
        );
    }

    #[test]
    fn absent_overlay_hint_preserves_configured_file_policy() {
        let mut config = pvisor_core::overlay::OverlayConfig {
            access_policy: pvisor_core::FileAccessPolicy::new(vec!["**/.ssh".into()], vec![])
                .unwrap(),
            ..Default::default()
        };
        super::apply_overlay_override(&mut config, &super::OverlayHint::default());
        assert_eq!(config.access_policy.deny(), ["**/.ssh"]);
        super::apply_overlay_override(
            &mut config,
            &super::OverlayHint {
                stage_dir: Some("/stage".into()),
                ..Default::default()
            },
        );
        assert!(config.access_policy.deny().is_empty());
    }

    #[test]
    fn gateway_loopback_urls_and_embedded_arguments_are_rewritten() {
        let mut spec = RunSpec::process("run-1", "agent", "codex");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process
            .env
            .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:19081/v1".into());
        process
            .args
            .push("openai_base_url=\"http://127.0.0.1:19081/v1\"".into());

        rewrite_vm_gateway_implant(&mut spec, "127.0.0.1:19081");

        let RunInvocation::Process(process) = &spec.invocation;
        assert_eq!(
            process.env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://192.0.2.1:19081/v1")
        );
        assert_eq!(
            process.args.last().map(String::as_str),
            Some("openai_base_url=\"http://192.0.2.1:19081/v1\"")
        );
        assert!(process.env["NO_PROXY"].contains("192.0.2.1"));
    }
}

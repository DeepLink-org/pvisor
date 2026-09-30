//! The task service (ttrpc `containerd.task.v2.Task`) and the shim
//! bootstrap (`containerd_shim::Shim`) for `io.containerd.pvisor.v2`.
//!
//! M2 scope: the host process path with full task IO ownership — create/
//! start/kill/wait/delete/state/pids/connect/shutdown for init processes,
//! exec (`Exec` -> `Start(exec_id)` -> `Wait`/`Kill`/`Delete` with
//! `TaskExecAdded`/`TaskExecStarted` events), `CloseIO` (stdin keepalive)
//! and `ResizePty` (retained PTY master). Stats, pause/resume and
//! checkpointing are not implemented yet (the generated trait defaults
//! report them as unsupported).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use containerd_shim::asynchronous::ExitSignal;
use containerd_shim::asynchronous::monitor::{Subscription, monitor_subscribe};
use containerd_shim::monitor::{ExitEvent, Subject, Topic};
use containerd_shim::{Config, Flags, Shim, StartOpts, TtrpcContext};
use containerd_shim_protos::api::{
    CloseIORequest, ConnectRequest, ConnectResponse, CreateTaskRequest, CreateTaskResponse,
    DeleteRequest, DeleteResponse, Empty, ExecProcessRequest, KillRequest, Mount, PidsRequest,
    PidsResponse, ProcessInfo, ResizePtyRequest, ShutdownRequest, StartRequest, StartResponse,
    StateRequest, StateResponse, Status, WaitRequest, WaitResponse,
};
use containerd_shim_protos::events::task::{
    TaskCreate, TaskDelete, TaskExecAdded, TaskExecStarted, TaskExit, TaskStart,
};
use containerd_shim_protos::protobuf::MessageDyn;
use containerd_shim_protos::protobuf::well_known_types::timestamp::Timestamp;
use containerd_shim_protos::shim_async::Task;
use containerd_shim_protos::topics::{
    TASK_CREATE_EVENT_TOPIC, TASK_DELETE_EVENT_TOPIC, TASK_EXEC_ADDED_EVENT_TOPIC,
    TASK_EXEC_STARTED_EVENT_TOPIC, TASK_EXIT_EVENT_TOPIC, TASK_START_EVENT_TOPIC,
};
use containerd_shim_protos::ttrpc::{self, Code, context::Context, get_status};
use log::{info, warn};
use tokio::sync::watch;

use crate::child::{self, InternalChild, StdioFds};
use crate::fifo::ContainerIo;
use crate::plan::{IoPlan, MountPlan, build_exec_plan, build_plan};
use crate::spec::load_bundle_spec;
use crate::state::{ExecEntry, ExitInfo, ExitTarget, TaskEntry, TaskStatus};

/// One live exec process: bookkeeping lives on the task entry; here we keep
/// the start pipe and the shim-owned IO.
struct LiveExec {
    exit_tx: watch::Sender<Option<ExitInfo>>,
    internal: Option<InternalChild>,
    io: ContainerIo,
}

/// One live task: init bookkeeping plus its start pipe, IO, and execs.
struct LiveTask {
    entry: TaskEntry,
    /// Exit channel for the init process; sends `Some(ExitInfo)` once.
    exit_tx: watch::Sender<Option<ExitInfo>>,
    internal: Option<InternalChild>,
    io: ContainerIo,
    execs: HashMap<String, LiveExec>,
}

struct TaskInner {
    namespace: String,
    publisher: containerd_shim::asynchronous::publisher::RemotePublisher,
    tasks: Mutex<HashMap<String, LiveTask>>,
    exit: Arc<ExitSignal>,
}

pub struct PvisorTask {
    inner: Arc<TaskInner>,
}

impl PvisorTask {
    async fn publish(&self, topic: &str, event: Box<dyn MessageDyn>) {
        if let Err(error) = self
            .inner
            .publisher
            .publish(Context::default(), topic, &self.inner.namespace, event)
            .await
        {
            warn!("publish {topic} failed: {error}");
        }
    }

    fn resolve_bundle(bundle: &str) -> Result<PathBuf> {
        let path = PathBuf::from(bundle);
        if path.is_absolute() {
            return Ok(path);
        }
        let cwd = std::env::current_dir().context("current dir")?;
        Ok(cwd.join(path))
    }

    fn convert_mounts(request_mounts: &[Mount]) -> Vec<MountPlan> {
        request_mounts
            .iter()
            .map(|mount| {
                let source = mount.source();
                // containerd sends the rootfs mount with an empty target;
                // it means "the root of the bundle rootfs".
                let target = mount.target();
                let destination = if target.is_empty() {
                    PathBuf::from("/")
                } else {
                    PathBuf::from(target)
                };
                MountPlan {
                    destination,
                    fs_type: mount.type_().to_string(),
                    source: if source.is_empty() {
                        None
                    } else {
                        Some(source.to_string())
                    },
                    options: mount.options().to_vec(),
                    from_request: true,
                }
            })
            .collect()
    }

    fn stdio_fds(io: &ContainerIo) -> StdioFds {
        let (stdin, stdout, stderr) = io.child_fds();
        StdioFds {
            stdin,
            stdout,
            stderr,
            terminal: io.is_terminal(),
        }
    }

    async fn delete_exec(&self, id: String, exec_id: String) -> ttrpc::Result<DeleteResponse> {
        let (entry, mut live_exec, bundle) = {
            let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
            let Some(live) = tasks.get_mut(&id) else {
                return Err(not_found(&id));
            };
            let Some(entry) = live.entry.execs.get(&exec_id) else {
                return Err(not_found(&exec_id));
            };
            if entry.status != TaskStatus::Stopped {
                return Err(rpc_error(
                    Code::FAILED_PRECONDITION,
                    format!("exec {exec_id} is not stopped"),
                ));
            }
            let entry = live.entry.execs.remove(&exec_id).expect("checked above");
            let live_exec = live.execs.remove(&exec_id).expect("entries stay in sync");
            (entry, live_exec, live.entry.bundle.clone())
        };

        if let Some(mut internal) = live_exec.internal.take() {
            internal.close_start();
        }
        let _ = std::fs::remove_file(bundle.join(format!("pvisor-exec-{exec_id}.json")));

        let mut response = DeleteResponse::new();
        response.set_pid(entry.pid.unwrap_or(0));
        if let Some(exit) = entry.exit.as_ref() {
            response.set_exit_status(exit.status);
            response.set_exited_at(timestamp_from(exit.exited_at));
        }

        let mut event = TaskDelete::new();
        event.set_container_id(id);
        event.set_pid(response.pid);
        event.set_exited_at(response.exited_at.clone().into_option().unwrap_or_default());
        self.publish(TASK_DELETE_EVENT_TOPIC, Box::new(event)).await;
        Ok(response)
    }
}

fn timestamp_from(system_time: SystemTime) -> Timestamp {
    let since = system_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let mut timestamp = Timestamp::new();
    timestamp.seconds = since.as_secs() as i64;
    timestamp.nanos = since.subsec_nanos() as i32;
    timestamp
}

fn rpc_error(code: Code, message: impl Into<String>) -> ttrpc::Error {
    ttrpc::Error::RpcStatus(get_status(code, message.into()))
}

fn not_found(id: &str) -> ttrpc::Error {
    rpc_error(Code::NOT_FOUND, format!("task {id} not found"))
}

fn status_to_api(status: TaskStatus) -> Status {
    match status {
        TaskStatus::Created => Status::CREATED,
        TaskStatus::Running => Status::RUNNING,
        TaskStatus::Stopped => Status::STOPPED,
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn io_plan_from(terminal: bool, stdin: &str, stdout: &str, stderr: &str) -> IoPlan {
    IoPlan {
        terminal,
        stdin: non_empty(stdin),
        stdout: non_empty(stdout),
        stderr: non_empty(stderr),
    }
}

/// Background loop mapping framework exit events onto tracked processes.
async fn run_exit_watcher(inner: Arc<TaskInner>, mut subscription: Subscription) {
    loop {
        let Some(event) = subscription.rx.recv().await else {
            return;
        };
        let ExitEvent {
            subject: Subject::Pid(pid),
            exit_code,
        } = event
        else {
            continue;
        };
        let exit = ExitInfo {
            status: exit_code as u32,
            exited_at: SystemTime::now(),
        };
        let notified = {
            let mut tasks = inner.tasks.lock().expect("tasks mutex");
            let mut found = None;
            for (id, live) in tasks.iter_mut() {
                match live
                    .entry
                    .record_exit_by_pid(pid as u32, exit.status, exit.exited_at)
                {
                    Some(ExitTarget::Init) => {
                        let _ = live.exit_tx.send(Some(exit));
                        found = Some((id.clone(), String::new()));
                        break;
                    }
                    Some(ExitTarget::Exec(exec_id)) => {
                        if let Some(exec) = live.execs.get_mut(&exec_id) {
                            let _ = exec.exit_tx.send(Some(exit));
                        }
                        found = Some((id.clone(), exec_id));
                        break;
                    }
                    None => {}
                }
            }
            found
        };
        let Some((container_id, exec_id)) = notified else {
            continue;
        };
        let mut event = TaskExit::new();
        event.set_container_id(container_id);
        event.set_id(exec_id);
        event.set_pid(pid as u32);
        event.set_exit_status(exit.status);
        event.set_exited_at(timestamp_from(exit.exited_at));
        let task = PvisorTask {
            inner: inner.clone(),
        };
        task.publish(TASK_EXIT_EVENT_TOPIC, Box::new(event)).await;
    }
}

#[async_trait]
impl Task for PvisorTask {
    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: CreateTaskRequest,
    ) -> ttrpc::Result<CreateTaskResponse> {
        if req.id.is_empty() {
            return Err(rpc_error(Code::INVALID_ARGUMENT, "task id required"));
        }
        if req.bundle.is_empty() {
            return Err(rpc_error(Code::INVALID_ARGUMENT, "bundle required"));
        }
        if self
            .inner
            .tasks
            .lock()
            .expect("tasks mutex")
            .contains_key(&req.id)
        {
            return Err(rpc_error(
                Code::ALREADY_EXISTS,
                format!("task {} already exists", req.id),
            ));
        }

        let bundle = Self::resolve_bundle(&req.bundle)
            .map_err(|error| rpc_error(Code::INVALID_ARGUMENT, format!("{error:#}")))?;
        let spec = load_bundle_spec(&bundle)
            .map_err(|error| rpc_error(Code::INVALID_ARGUMENT, format!("{error:#}")))?;
        let io = io_plan_from(req.terminal, &req.stdin, &req.stdout, &req.stderr);
        let plan = build_plan(
            &spec,
            &req.id,
            &bundle,
            Self::convert_mounts(&req.rootfs),
            io,
        )
        .map_err(|error| rpc_error(Code::INVALID_ARGUMENT, error.to_string()))?;
        for warning in &plan.warnings {
            warn!("task {}: {warning}", req.id);
        }

        // The shim owns the task IO; descriptors travel to the init child
        // through fd inheritance.
        let mut owned_io = ContainerIo::open(&plan.io)
            .map_err(|error| rpc_error(Code::INTERNAL, format!("open task io: {error:#}")))?;
        let stdio = Self::stdio_fds(&owned_io);
        let plan_path = bundle.join("pvisor-plan.json");
        let plan_bytes = serde_json::to_vec(&plan)
            .map_err(|error| rpc_error(Code::INTERNAL, format!("{error}")))?;

        let internal = tokio::task::spawn_blocking({
            let plan_path = plan_path.clone();
            move || {
                child::spawn_internal(
                    child::INTERNAL_INIT_ARG,
                    &plan_path,
                    &plan_bytes,
                    Some(stdio),
                )
            }
        })
        .await
        .map_err(|error| rpc_error(Code::INTERNAL, format!("init join: {error}")))?
        .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
        let pid = internal.pid.unwrap_or(0);
        // The child owns its descriptors now; keep only keepalive/master.
        owned_io.release_child_fds();

        let entry = TaskEntry::new(
            &req.id,
            bundle,
            plan.io.stdin.clone(),
            plan.io.stdout.clone(),
            plan.io.stderr.clone(),
            plan.io.terminal,
            Some(pid),
        );
        let (exit_tx, _) = watch::channel(None);
        self.inner.tasks.lock().expect("tasks mutex").insert(
            req.id.clone(),
            LiveTask {
                entry,
                exit_tx,
                internal: Some(internal),
                io: owned_io,
                execs: HashMap::new(),
            },
        );

        let mut event = TaskCreate::new();
        event.set_container_id(req.id.clone());
        event.set_bundle(plan.bundle.to_string_lossy().to_string());
        event.set_pid(pid);
        self.publish(TASK_CREATE_EVENT_TOPIC, Box::new(event)).await;

        let mut response = CreateTaskResponse::new();
        response.set_pid(pid);
        Ok(response)
    }

    async fn start(&self, _ctx: &TtrpcContext, req: StartRequest) -> ttrpc::Result<StartResponse> {
        let (pid, exec_id) = {
            let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get_mut(&req.id).ok_or_else(|| not_found(&req.id))?;
            if !req.exec_id.is_empty() {
                let mut internal = live
                    .execs
                    .get_mut(&req.exec_id)
                    .and_then(|exec| exec.internal.take())
                    .ok_or_else(|| {
                        rpc_error(
                            Code::FAILED_PRECONDITION,
                            format!("exec {} already started", req.exec_id),
                        )
                    })?;
                internal
                    .start()
                    .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
                let entry = live
                    .entry
                    .execs
                    .get_mut(&req.exec_id)
                    .ok_or_else(|| not_found(&req.exec_id))?;
                let pid = entry.pid.unwrap_or(0);
                if !entry.mark_started(pid) {
                    return Err(rpc_error(
                        Code::FAILED_PRECONDITION,
                        format!("exec {} cannot start", req.exec_id),
                    ));
                }
                (pid, req.exec_id.clone())
            } else {
                let mut internal = live
                    .internal
                    .take()
                    .ok_or_else(|| rpc_error(Code::FAILED_PRECONDITION, "task already started"))?;
                internal
                    .start()
                    .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
                let pid = live.entry.pid.unwrap_or(0);
                if !live.entry.mark_started(pid) {
                    return Err(rpc_error(
                        Code::FAILED_PRECONDITION,
                        format!("task {} cannot start", req.id),
                    ));
                }
                (pid, String::new())
            }
        };

        if exec_id.is_empty() {
            let mut event = TaskStart::new();
            event.set_container_id(req.id.clone());
            event.set_pid(pid);
            self.publish(TASK_START_EVENT_TOPIC, Box::new(event)).await;
        } else {
            let mut event = TaskExecStarted::new();
            event.set_container_id(req.id.clone());
            event.set_exec_id(exec_id);
            event.set_pid(pid);
            self.publish(TASK_EXEC_STARTED_EVENT_TOPIC, Box::new(event))
                .await;
        }

        let mut response = StartResponse::new();
        response.set_pid(pid);
        Ok(response)
    }

    async fn state(&self, _ctx: &TtrpcContext, req: StateRequest) -> ttrpc::Result<StateResponse> {
        let tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
        if !req.exec_id.is_empty() {
            let exec = live
                .entry
                .execs
                .get(&req.exec_id)
                .ok_or_else(|| not_found(&req.exec_id))?;
            let mut response = StateResponse::new();
            response.set_id(req.id.clone());
            response.set_exec_id(req.exec_id.clone());
            response.set_bundle(live.entry.bundle.to_string_lossy().to_string());
            response.set_pid(exec.pid.unwrap_or(0));
            response.set_status(status_to_api(exec.status));
            response.set_stdin(exec.stdin.clone().unwrap_or_default());
            response.set_stdout(exec.stdout.clone().unwrap_or_default());
            response.set_stderr(exec.stderr.clone().unwrap_or_default());
            response.set_terminal(exec.terminal);
            if let Some(exit) = exec.exit.as_ref() {
                response.set_exit_status(exit.status);
                response.set_exited_at(timestamp_from(exit.exited_at));
            }
            return Ok(response);
        }
        let entry = &live.entry;
        let mut response = StateResponse::new();
        response.set_id(entry.id.clone());
        response.set_bundle(entry.bundle.to_string_lossy().to_string());
        response.set_pid(entry.pid.unwrap_or(0));
        response.set_status(status_to_api(entry.status));
        response.set_stdin(entry.stdin.clone().unwrap_or_default());
        response.set_stdout(entry.stdout.clone().unwrap_or_default());
        response.set_stderr(entry.stderr.clone().unwrap_or_default());
        response.set_terminal(entry.terminal);
        if let Some(exit) = entry.exit.as_ref() {
            response.set_exit_status(exit.status);
            response.set_exited_at(timestamp_from(exit.exited_at));
        }
        Ok(response)
    }

    async fn wait(&self, _ctx: &TtrpcContext, req: WaitRequest) -> ttrpc::Result<WaitResponse> {
        let mut receiver = {
            let tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
            if req.exec_id.is_empty() {
                live.exit_tx.subscribe()
            } else {
                live.execs
                    .get(&req.exec_id)
                    .ok_or_else(|| not_found(&req.exec_id))?
                    .exit_tx
                    .subscribe()
            }
        };
        loop {
            if let Some(exit) = receiver.borrow().as_ref() {
                let mut response = WaitResponse::new();
                response.set_exit_status(exit.status);
                response.set_exited_at(timestamp_from(exit.exited_at));
                return Ok(response);
            }
            if receiver.changed().await.is_err() {
                // Sender dropped: the process was deleted while we waited.
                return Err(not_found(&req.id));
            }
        }
    }

    async fn exec(&self, _ctx: &TtrpcContext, req: ExecProcessRequest) -> ttrpc::Result<Empty> {
        if req.exec_id.is_empty() {
            return Err(rpc_error(Code::INVALID_ARGUMENT, "exec id required"));
        }
        if req.spec.is_none() {
            return Err(rpc_error(
                Code::INVALID_ARGUMENT,
                "exec process spec required",
            ));
        }
        let (bundle, init_pid) = {
            let tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
            if live.entry.status != TaskStatus::Running {
                return Err(rpc_error(
                    Code::FAILED_PRECONDITION,
                    format!("task {} is not running", req.id),
                ));
            }
            if live.entry.execs.contains_key(&req.exec_id) {
                return Err(rpc_error(
                    Code::ALREADY_EXISTS,
                    format!("exec {} already exists", req.exec_id),
                ));
            }
            (live.entry.bundle.clone(), live.entry.pid.unwrap_or(0))
        };

        // The Any payload carries the JSON-encoded OCI process spec.
        let process: oci_spec::runtime::Process = serde_json::from_slice(&req.spec().value)
            .map_err(|error| {
                rpc_error(
                    Code::INVALID_ARGUMENT,
                    format!("invalid exec process spec: {error}"),
                )
            })?;
        let io = io_plan_from(req.terminal, &req.stdin, &req.stdout, &req.stderr);
        let plan = build_exec_plan(&process, &req.id, &req.exec_id, init_pid, io)
            .map_err(|error| rpc_error(Code::INVALID_ARGUMENT, error.to_string()))?;

        let mut owned_io = ContainerIo::open(&plan.io)
            .map_err(|error| rpc_error(Code::INTERNAL, format!("open exec io: {error:#}")))?;
        let stdio = Self::stdio_fds(&owned_io);
        let plan_path = bundle.join(format!("pvisor-exec-{}.json", req.exec_id));
        let plan_bytes = serde_json::to_vec(&plan)
            .map_err(|error| rpc_error(Code::INTERNAL, format!("serialize exec plan: {error}")))?;

        let internal = tokio::task::spawn_blocking({
            let plan_path = plan_path.clone();
            move || {
                child::spawn_internal(
                    child::INTERNAL_EXEC_ARG,
                    &plan_path,
                    &plan_bytes,
                    Some(stdio),
                )
            }
        })
        .await
        .map_err(|error| rpc_error(Code::INTERNAL, format!("exec join: {error}")))?
        .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
        let pid = internal.pid.unwrap_or(0);
        owned_io.release_child_fds();

        let exec_entry = ExecEntry::new(
            &req.exec_id,
            plan.io.stdin.clone(),
            plan.io.stdout.clone(),
            plan.io.stderr.clone(),
            plan.io.terminal,
            Some(pid),
        );
        let (exit_tx, _) = watch::channel(None);
        {
            let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get_mut(&req.id).ok_or_else(|| not_found(&req.id))?;
            if !live.entry.add_exec(exec_entry) {
                return Err(rpc_error(
                    Code::ALREADY_EXISTS,
                    format!("exec {} already exists", req.exec_id),
                ));
            }
            live.execs.insert(
                req.exec_id.clone(),
                LiveExec {
                    exit_tx,
                    internal: Some(internal),
                    io: owned_io,
                },
            );
        }

        let mut event = TaskExecAdded::new();
        event.set_container_id(req.id.clone());
        event.set_exec_id(req.exec_id.clone());
        self.publish(TASK_EXEC_ADDED_EVENT_TOPIC, Box::new(event))
            .await;
        Ok(Empty::new())
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: KillRequest) -> ttrpc::Result<Empty> {
        let signal = req.signal as i32;
        if !(1..=64).contains(&signal) {
            return Err(rpc_error(
                Code::INVALID_ARGUMENT,
                format!("invalid signal {}", req.signal),
            ));
        }
        let pid = {
            let tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
            if req.exec_id.is_empty() {
                live.entry.pid
            } else {
                live.entry
                    .execs
                    .get(&req.exec_id)
                    .ok_or_else(|| not_found(&req.exec_id))?
                    .pid
            }
        };
        let Some(pid) = pid else {
            return Ok(Empty::new());
        };
        // `all` targets the process group; each process detached into one.
        let target = if req.all { -(pid as i32) } else { pid as i32 };
        if unsafe { libc::kill(target, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            // ESRCH after exit is benign: the process already went away.
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(rpc_error(
                    Code::INTERNAL,
                    format!("kill {target} with {signal}: {error}"),
                ));
            }
        }
        Ok(Empty::new())
    }

    async fn delete(
        &self,
        _ctx: &TtrpcContext,
        req: DeleteRequest,
    ) -> ttrpc::Result<DeleteResponse> {
        if !req.exec_id.is_empty() {
            return self.delete_exec(req.id, req.exec_id).await;
        }
        let live = {
            let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
            let Some(live) = tasks.get_mut(&req.id) else {
                return Err(not_found(&req.id));
            };
            if !live.entry.can_delete() {
                return Err(rpc_error(
                    Code::FAILED_PRECONDITION,
                    format!("task {} is not stopped", req.id),
                ));
            }
            tasks.remove(&req.id).expect("entry checked above")
        };

        let mut response = DeleteResponse::new();
        response.set_pid(live.entry.pid.unwrap_or(0));
        if let Some(exit) = live.entry.exit.as_ref() {
            response.set_exit_status(exit.status);
            response.set_exited_at(timestamp_from(exit.exited_at));
        }

        // Release a created-never-started init (EOF makes it exit) and drop
        // the serialized plan from the bundle.
        if let Some(mut internal) = live.internal {
            internal.close_start();
        }
        let _ = std::fs::remove_file(live.entry.bundle.join("pvisor-plan.json"));

        let mut event = TaskDelete::new();
        event.set_container_id(req.id.clone());
        event.set_pid(response.pid);
        event.set_exited_at(response.exited_at.clone().into_option().unwrap_or_default());
        self.publish(TASK_DELETE_EVENT_TOPIC, Box::new(event)).await;
        Ok(response)
    }

    async fn pids(&self, _ctx: &TtrpcContext, req: PidsRequest) -> ttrpc::Result<PidsResponse> {
        let tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
        let mut response = PidsResponse::new();
        if let Some(pid) = live.entry.pid {
            let mut process = ProcessInfo::new();
            process.set_pid(pid);
            response.processes.push(process);
        }
        for exec in live.entry.execs.values() {
            if let Some(pid) = exec.pid {
                let mut process = ProcessInfo::new();
                process.set_pid(pid);
                response.processes.push(process);
            }
        }
        Ok(response)
    }

    async fn connect(
        &self,
        _ctx: &TtrpcContext,
        req: ConnectRequest,
    ) -> ttrpc::Result<ConnectResponse> {
        let tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
        let mut response = ConnectResponse::new();
        response.set_shim_pid(process::id());
        response.set_task_pid(live.entry.pid.unwrap_or(0));
        Ok(response)
    }

    async fn close_io(&self, _ctx: &TtrpcContext, req: CloseIORequest) -> ttrpc::Result<Empty> {
        if !req.stdin {
            return Ok(Empty::new());
        }
        let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get_mut(&req.id).ok_or_else(|| not_found(&req.id))?;
        if req.exec_id.is_empty() {
            live.io.close_stdin();
        } else if let Some(exec) = live.execs.get_mut(&req.exec_id) {
            exec.io.close_stdin();
        } else {
            return Err(not_found(&req.exec_id));
        }
        Ok(Empty::new())
    }

    async fn resize_pty(&self, _ctx: &TtrpcContext, req: ResizePtyRequest) -> ttrpc::Result<Empty> {
        let tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
        let io = if req.exec_id.is_empty() {
            &live.io
        } else {
            &live
                .execs
                .get(&req.exec_id)
                .ok_or_else(|| not_found(&req.exec_id))?
                .io
        };
        io.resize(req.width, req.height)
            .map_err(|error| rpc_error(Code::FAILED_PRECONDITION, format!("{error:#}")))?;
        Ok(Empty::new())
    }

    async fn shutdown(&self, _ctx: &TtrpcContext, _req: ShutdownRequest) -> ttrpc::Result<Empty> {
        let task_count = self.inner.tasks.lock().expect("tasks mutex").len();
        if task_count == 0 {
            info!("shim shutdown requested with no live tasks");
            self.inner.exit.signal();
        } else {
            info!("shim shutdown deferred: {task_count} task(s) still tracked");
        }
        Ok(Empty::new())
    }
}

/// Shim bootstrap: containerd's `start`/`delete` CLI actions plus task
/// service construction.
pub struct PvisorShim {
    id: String,
    namespace: String,
    exit: Arc<ExitSignal>,
}

#[async_trait]
impl Shim for PvisorShim {
    type T = PvisorTask;

    async fn new(runtime_id: &str, args: &Flags, _config: &mut Config) -> Self {
        info!(
            "creating {} shim instance (container {})",
            runtime_id, args.id
        );
        PvisorShim {
            id: args.id.clone(),
            namespace: args.namespace.clone(),
            exit: Arc::new(ExitSignal::default()),
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> containerd_shim::Result<String> {
        let ttrpc_address = opts.ttrpc_address.clone();
        let vars = vec![("TTRPC_ADDRESS", ttrpc_address.as_str())];
        containerd_shim::asynchronous::spawn(opts, &self.id, vars).await
    }

    async fn delete_shim(&mut self) -> containerd_shim::Result<DeleteResponse> {
        info!("shim cleanup for {}", self.id);
        Ok(DeleteResponse::new())
    }

    async fn wait(&mut self) {
        self.exit.wait().await;
    }

    async fn create_task_service(
        &self,
        publisher: containerd_shim::asynchronous::publisher::RemotePublisher,
    ) -> Self::T {
        let inner = Arc::new(TaskInner {
            namespace: self.namespace.clone(),
            publisher,
            tasks: Mutex::new(HashMap::new()),
            exit: self.exit.clone(),
        });
        match monitor_subscribe(Topic::Pid).await {
            Ok(subscription) => {
                let watcher_inner = inner.clone();
                tokio::spawn(async move {
                    run_exit_watcher(watcher_inner, subscription).await;
                });
            }
            Err(error) => warn!("exit monitor unavailable: {error}"),
        }
        PvisorTask { inner }
    }
}

/// Binary entry: run the shim bootstrap inside a tokio runtime.
pub fn shim_main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        containerd_shim::run::<PvisorShim>(crate::RUNTIME_TYPE, None).await;
    });
}

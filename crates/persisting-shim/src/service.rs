//! The task service (ttrpc `containerd.task.v2.Task`) and the shim
//! bootstrap (`containerd_shim::Shim`) for `io.containerd.pvisor.v2`.
//!
//! M1 scope: the host process path. `Create` builds a [`ContainerPlan`],
//! re-execs the init parent, and tracks the task; `Start` releases the init
//! process; exits arrive through the framework's SIGCHLD monitor. Exec,
//! stats, pty resize, pause/resume and checkpointing are not implemented
//! yet (the generated trait defaults report them as unsupported).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use containerd_shim::TtrpcContext;
use containerd_shim::asynchronous::ExitSignal;
use containerd_shim::asynchronous::monitor::{Subscription, monitor_subscribe};
use containerd_shim::monitor::{ExitEvent, Subject, Topic};
use containerd_shim::{Config, Flags, Shim, StartOpts};
use containerd_shim_protos::api::{
    ConnectRequest, ConnectResponse, CreateTaskRequest, CreateTaskResponse, DeleteRequest,
    DeleteResponse, Empty, KillRequest, Mount, PidsRequest, PidsResponse, ProcessInfo,
    ShutdownRequest, StartRequest, StartResponse, StateRequest, StateResponse, Status, WaitRequest,
    WaitResponse,
};
use containerd_shim_protos::events::task::{TaskCreate, TaskDelete, TaskExit, TaskStart};
use containerd_shim_protos::protobuf::MessageDyn;
use containerd_shim_protos::protobuf::well_known_types::timestamp::Timestamp;
use containerd_shim_protos::shim_async::Task;
use containerd_shim_protos::topics::{
    TASK_CREATE_EVENT_TOPIC, TASK_DELETE_EVENT_TOPIC, TASK_EXIT_EVENT_TOPIC, TASK_START_EVENT_TOPIC,
};
use containerd_shim_protos::ttrpc::{self, Code, context::Context, get_status};
use log::{info, warn};
use tokio::sync::watch;

use crate::child::{self, InitChild};
use crate::plan::{IoPlan, MountPlan, build_plan};
use crate::spec::load_bundle_spec;
use crate::state::{ExitInfo, TaskEntry, TaskStatus};

/// One live task: bookkeeping entry plus the start pipe until Start.
struct LiveTask {
    entry: TaskEntry,
    /// Exit channel; sends `Some(ExitInfo)` exactly once.
    exit_tx: watch::Sender<Option<ExitInfo>>,
    init: Option<InitChild>,
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

/// Background loop mapping framework exit events onto tracked tasks.
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
        let exited = {
            let mut tasks = inner.tasks.lock().expect("tasks mutex");
            let mut found = None;
            for (id, live) in tasks.iter_mut() {
                if live.entry.pid == Some(pid as u32)
                    && live.entry.mark_exited(exit.status, exit.exited_at)
                {
                    let _ = live.exit_tx.send(Some(exit));
                    found = Some(id.clone());
                }
            }
            found
        };
        let Some(id) = exited else {
            continue;
        };
        let mut event = TaskExit::new();
        event.set_container_id(id.clone());
        event.set_id(id);
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
        let io = IoPlan {
            terminal: req.terminal,
            stdin: non_empty(&req.stdin),
            stdout: non_empty(&req.stdout),
            stderr: non_empty(&req.stderr),
        };
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

        let init = tokio::task::spawn_blocking({
            let plan = plan.clone();
            move || child::spawn_init_child(&plan)
        })
        .await
        .map_err(|error| rpc_error(Code::INTERNAL, format!("init join: {error}")))?
        .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
        let pid = init.pid.unwrap_or(0);

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
                init: Some(init),
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
        if !req.exec_id.is_empty() {
            return Err(rpc_error(Code::UNIMPLEMENTED, "exec is not supported yet"));
        }
        let pid = {
            let mut tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get_mut(&req.id).ok_or_else(|| not_found(&req.id))?;
            let mut init = live
                .init
                .take()
                .ok_or_else(|| rpc_error(Code::FAILED_PRECONDITION, "task already started"))?;
            init.start()
                .map_err(|error| rpc_error(Code::INTERNAL, format!("{error:#}")))?;
            let pid = live.entry.pid.unwrap_or(0);
            if !live.entry.mark_started(pid) {
                return Err(rpc_error(
                    Code::FAILED_PRECONDITION,
                    format!(
                        "task {} cannot start from state {:?}",
                        req.id, live.entry.status
                    ),
                ));
            }
            pid
        };

        let mut event = TaskStart::new();
        event.set_container_id(req.id.clone());
        event.set_pid(pid);
        self.publish(TASK_START_EVENT_TOPIC, Box::new(event)).await;

        let mut response = StartResponse::new();
        response.set_pid(pid);
        Ok(response)
    }

    async fn state(&self, _ctx: &TtrpcContext, req: StateRequest) -> ttrpc::Result<StateResponse> {
        let tasks = self.inner.tasks.lock().expect("tasks mutex");
        let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
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
            live.exit_tx.subscribe()
        };
        loop {
            if let Some(exit) = receiver.borrow().as_ref() {
                let mut response = WaitResponse::new();
                response.set_exit_status(exit.status);
                response.set_exited_at(timestamp_from(exit.exited_at));
                return Ok(response);
            }
            if receiver.changed().await.is_err() {
                // Sender dropped: the task was deleted while we waited.
                return Err(not_found(&req.id));
            }
        }
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: KillRequest) -> ttrpc::Result<Empty> {
        if !req.exec_id.is_empty() {
            return Err(rpc_error(Code::UNIMPLEMENTED, "exec is not supported yet"));
        }
        let pid = {
            let tasks = self.inner.tasks.lock().expect("tasks mutex");
            let live = tasks.get(&req.id).ok_or_else(|| not_found(&req.id))?;
            live.entry.pid
        };
        let Some(pid) = pid else {
            return Ok(Empty::new());
        };
        let signal = req.signal as i32;
        if !(1..=64).contains(&signal) {
            return Err(rpc_error(
                Code::INVALID_ARGUMENT,
                format!("invalid signal {}", req.signal),
            ));
        }
        // `all` targets the process group; the init detached into one.
        let target = if req.all { -(pid as i32) } else { pid as i32 };
        if unsafe { libc::kill(target, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            // ESRCH after exit is benign: the task already went away.
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
            return Err(rpc_error(Code::UNIMPLEMENTED, "exec is not supported yet"));
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
        if let Some(mut init) = live.init {
            init.close_start();
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

use clap::{Parser, Subcommand, ValueEnum};
use pvisor_cluster::{
    ArtifactGcRequest, CLUSTER_VERSION, client::Client, scheduler::SchedulerConfig,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "pVisor task controller with Worker-reconciled runtime state"
)]
struct Args {
    #[arg(
        long,
        default_value = "http://127.0.0.1:19800",
        env = "PVISOR_CLUSTER_URL"
    )]
    url: String,
    #[arg(long, env = "PVISOR_CLUSTER_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value = "127.0.0.1:19800")]
        listen: std::net::SocketAddr,
        #[arg(long, default_value = ".pvisor/cluster/journal")]
        journal: PathBuf,
        #[arg(long, env = "PVISOR_CLUSTER_WORKER_TOKEN", hide_env_values = true)]
        worker_token: String,
        #[arg(long, default_value_t = 30_000)]
        lease_ms: u64,
        /// JSON map of tenant names to concurrent Resources limits.
        #[arg(long)]
        quotas: Option<PathBuf>,
        /// Maximum retained controller WAL size; no history is silently deleted.
        #[arg(long, default_value_t = 1024 * 1024 * 1024)]
        max_journal_bytes: u64,
        /// Maximum retained artifact payload bytes (including orphan uploads).
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
        max_artifact_bytes: u64,
        /// JSON unique-object storage limits; persisted across controller restart.
        #[arg(long)]
        artifact_limits: Option<PathBuf>,
    },
    Submit {
        spec: PathBuf,
    },
    /// Submit, inspect or cancel a durable task dependency graph.
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    /// Atomically create branches from a sealed full execution checkpoint.
    Fork {
        source: String,
        /// JSON ExecutionForkRequest; reuse its request_id after a timeout.
        request: PathBuf,
    },
    /// Show the durable creation receipt, independent of branch execution.
    ShowFork {
        source: String,
        request_id: String,
    },
    /// Capture a running VM and durably create branches after acknowledgement.
    ForkLive {
        source: String,
        /// JSON ExecutionForkRequest with a fresh checkpoint_request_id.
        request: PathBuf,
    },
    /// Show capture progress and the eventual branch creation receipt.
    ShowLiveFork {
        source: String,
        request_id: String,
    },
    /// Register or inspect an immutable native-cache environment template.
    Environment {
        #[command(subcommand)]
        command: EnvironmentCommand,
    },
    Show {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// Fence an unreachable execution awaiting restart reconciliation; never retry it.
    ResolveLost {
        /// JSON LeaseKey copied from the task record; includes generation/incarnation.
        key: PathBuf,
    },
    /// Pause, offload, resume, checkpoint or suspend a VM. Reuse request-id after timeouts.
    Control {
        id: String,
        #[arg(value_enum)]
        action: Action,
        #[arg(long)]
        request_id: String,
    },
    Workers,
    /// Show retained native artifacts or download verified files to a directory.
    Artifacts {
        id: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Inspect this Linux node's read-only pressure and visible cgroup limits.
    ProbeNode,
    /// Show unique stored objects, concurrent reservations and persistent limits.
    ArtifactStorage {
        /// Persist a new policy online; omit to read current usage.
        #[arg(long)]
        limits: Option<PathBuf>,
    },
    /// Preview orphan reclamation and optional terminal evidence retirement.
    ArtifactGc {
        #[arg(long)]
        retire_before_ms: Option<u64>,
        #[arg(long, default_value_t = 4096)]
        max_objects: u32,
        /// Apply the immutable server plan ID printed by a previous preview.
        #[arg(long, conflicts_with = "retire_before_ms")]
        apply: Option<String>,
    },
    Drain {
        id: String,
        #[arg(long)]
        resume: bool,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum Action {
    Pause,
    Offload,
    Resume,
    Checkpoint,
    Suspend,
}
#[derive(Subcommand)]
enum EnvironmentCommand {
    Publish { template: PathBuf },
    Show { digest: String },
}
#[derive(Subcommand)]
enum GraphCommand {
    Submit { spec: PathBuf },
    Show { id: String },
    Cancel { id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if matches!(args.command, Command::ProbeNode) {
        println!(
            "{}",
            serde_json::to_string_pretty(&pvisor_cluster::admission::sample_linux()?)?
        );
        return Ok(());
    }
    let token = args
        .token
        .ok_or_else(|| anyhow::anyhow!("set PVISOR_CLUSTER_TOKEN or --token"))?;
    if let Command::Serve {
        listen,
        journal,
        worker_token,
        lease_ms,
        quotas,
        max_journal_bytes,
        max_artifact_bytes,
        artifact_limits,
    } = args.command
    {
        let mut config = SchedulerConfig {
            lease_duration_ms: lease_ms,
            max_journal_bytes,
            max_artifact_bytes,
            ..Default::default()
        };
        if let Some(path) = quotas {
            config.tenant_quotas = serde_json::from_slice(&std::fs::read(path)?)?;
        }
        if let Some(path) = artifact_limits {
            config.artifact_storage_limits = Some(serde_json::from_slice(&std::fs::read(path)?)?);
        }
        let scheduler = pvisor_cluster::scheduler::Scheduler::open(&journal, config)?;
        let router = pvisor_cluster::server::router(scheduler, token, worker_token)?;
        let listener = tokio::net::TcpListener::bind(listen).await?;
        eprintln!("pVisor controller listening on {}", listener.local_addr()?);
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        return Ok(());
    }
    let client = Client::new(&args.url, token)?;
    let value = match args.command {
        Command::ResolveLost { key } => {
            let key = serde_json::from_slice(&std::fs::read(key)?)?;
            serde_json::to_value(client.resolve_lost(&key).await?)?
        }
        Command::Graph { command } => serde_json::to_value(match command {
            GraphCommand::Submit { spec } => {
                client
                    .submit_graph(&serde_json::from_slice(&std::fs::read(spec)?)?)
                    .await?
            }
            GraphCommand::Show { id } => client.graph(&id).await?,
            GraphCommand::Cancel { id } => client.cancel_graph(&id).await?,
        })?,
        Command::Environment { command } => serde_json::to_value(match command {
            EnvironmentCommand::Publish { template } => {
                client
                    .publish_environment(&serde_json::from_slice(&std::fs::read(template)?)?)
                    .await?
            }
            EnvironmentCommand::Show { digest } => client.environment(&digest).await?,
        })?,
        Command::Submit { spec } => serde_json::to_value(
            client
                .submit(&serde_json::from_slice(&std::fs::read(spec)?)?)
                .await?,
        )?,
        Command::Show { id } => serde_json::to_value(client.task(&id).await?)?,
        Command::Fork { source, request } => serde_json::to_value(
            client
                .fork_execution(&source, &serde_json::from_slice(&std::fs::read(request)?)?)
                .await?,
        )?,
        Command::ShowFork { source, request_id } => {
            serde_json::to_value(client.execution_fork(&source, &request_id).await?)?
        }
        Command::Cancel { id } => serde_json::to_value(client.cancel(&id).await?)?,
        Command::ForkLive { source, request } => serde_json::to_value(
            client
                .request_live_fork(&source, &serde_json::from_slice(&std::fs::read(request)?)?)
                .await?,
        )?,
        Command::ShowLiveFork { source, request_id } => {
            serde_json::to_value(client.live_fork(&source, &request_id).await?)?
        }
        Command::Control {
            id,
            action,
            request_id,
        } => serde_json::to_value(
            client
                .control(
                    &id,
                    &pvisor_cluster::ControlRequest {
                        request_id,
                        action: match action {
                            Action::Pause => pvisor_cluster::ControlAction::Pause,
                            Action::Offload => pvisor_cluster::ControlAction::Offload,
                            Action::Resume => pvisor_cluster::ControlAction::Resume,
                            Action::Checkpoint => pvisor_cluster::ControlAction::Checkpoint,
                            Action::Suspend => pvisor_cluster::ControlAction::Suspend,
                        },
                    },
                )
                .await?,
        )?,
        Command::ArtifactGc {
            retire_before_ms,
            max_objects,
            apply,
        } => match apply {
            Some(id) => serde_json::to_value(client.apply_artifact_gc(&id).await?)?,
            None => serde_json::to_value(
                client
                    .plan_artifact_gc(&ArtifactGcRequest {
                        version: CLUSTER_VERSION,
                        retire_before_ms,
                        max_objects,
                    })
                    .await?,
            )?,
        },
        Command::ArtifactStorage { limits } => serde_json::to_value(match limits {
            Some(path) => {
                client
                    .update_artifact_storage(&serde_json::from_slice(&std::fs::read(path)?)?)
                    .await?
            }
            None => client.artifact_storage().await?,
        })?,
        Command::Workers => serde_json::to_value(client.workers().await?)?,
        Command::Artifacts { id, out } => serde_json::to_value(match out {
            Some(path) => client.download_artifacts(&id, &path).await?,
            None => client.artifacts(&id).await?,
        })?,
        Command::Drain { id, resume } => client.drain(&id, !resume).await?,
        Command::Serve { .. } => unreachable!(),
        Command::ProbeNode => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

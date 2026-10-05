use clap::{Parser, Subcommand, ValueEnum};
use pvisor_cluster::{client::Client, scheduler::SchedulerConfig};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Durable distributed pVisor task controller")]
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
    },
    Submit {
        spec: PathBuf,
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
    /// Pause, offload, or resume a leased VM. Reuse request-id after timeouts.
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
}
#[derive(Subcommand)]
enum EnvironmentCommand {
    Publish { template: PathBuf },
    Show { digest: String },
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
        let router = pvisor_cluster::server::open(&journal, config, token, worker_token)?;
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
        Command::Cancel { id } => serde_json::to_value(client.cancel(&id).await?)?,
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
                        },
                    },
                )
                .await?,
        )?,
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

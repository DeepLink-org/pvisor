use clap::{Parser, Subcommand};
use pvisor_daemon::{
    daemon::{self, Config, Daemon},
    runtime::PodmanRuntime,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "Single-node pVisor sandbox daemon (OpenSandbox 1.1.0)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve sandbox lifecycle and stream the prepared image's real execd API.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Externally reachable host:port, without scheme/path. Required behind a proxy.
        #[arg(long)]
        public_endpoint: Option<String>,
        #[arg(long, default_value = ".pvisor/daemon")]
        state: PathBuf,
        #[arg(long, env = "OPEN_SANDBOX_API_KEY", hide_env_values = true)]
        api_key: String,
        /// Absolute path to the trusted local rootless Podman executable.
        #[arg(long)]
        podman: PathBuf,
        #[arg(long, default_value_t = 32)]
        max_sandboxes: usize,
        /// Total admitted CPU quota, in thousandths of one CPU. No implicit overcommit.
        #[arg(long, default_value_t = 4000)]
        cpu_millis: u64,
        /// Total admitted hard memory limits; actual backing/cache remain runtime-accounted.
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
        memory_bytes: u64,
        #[arg(long, default_value_t = 86400)]
        max_timeout_seconds: u64,
    },
    /// Print the fixed protocol baseline, not a claim of full API support.
    Protocol,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Protocol => println!(
            "OpenSandbox {} ({})",
            daemon::OPENSANDBOX_VERSION,
            daemon::OPENSANDBOX_COMMIT
        ),
        Command::Serve {
            listen,
            public_endpoint,
            state,
            api_key,
            podman,
            max_sandboxes,
            cpu_millis,
            memory_bytes,
            max_timeout_seconds,
        } => {
            anyhow::ensure!(
                cfg!(target_os = "linux"),
                "rootless Podman backend requires Linux"
            );
            anyhow::ensure!(
                (60..=365 * 24 * 60 * 60).contains(&max_timeout_seconds),
                "timeout limit is too large"
            );
            anyhow::ensure!(
                (!listen.ip().is_unspecified() && listen.port() != 0) || public_endpoint.is_some(),
                "wildcard or ephemeral listen requires an explicit public endpoint"
            );
            let config = Config {
                state_dir: state,
                api_key,
                public_endpoint: public_endpoint.unwrap_or_else(|| listen.to_string()),
                max_sandboxes,
                cpu_millis,
                memory_bytes,
                max_timeout_seconds,
            };
            let daemon = Daemon::open(config, move |owner| {
                Ok(Arc::new(PodmanRuntime::new(podman, owner)?)
                    as Arc<dyn pvisor_daemon::runtime::Runtime>)
            })
            .await?;
            let maintenance = daemon.start_maintenance();
            let listener = tokio::net::TcpListener::bind(listen).await?;
            eprintln!(
                "pVisor daemon listening on {}; prepared execd/egress images required",
                listener.local_addr()?
            );
            let server = axum::serve(listener, daemon::api::router(daemon))
                .with_graceful_shutdown(shutdown());
            let result = server.await;
            maintenance.abort();
            result?;
        }
    }
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    // Containers and their durable records deliberately survive service restart.
}

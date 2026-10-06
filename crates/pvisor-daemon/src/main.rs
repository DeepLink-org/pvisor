use clap::{Parser, Subcommand};
use pvisor_daemon::{
    daemon::{self, Config, Daemon},
    runtime::{NativeRuntime, NativeRuntimeConfig, run_native_supervisor},
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "Single-node pVisor VM sandbox daemon (OpenSandbox 1.1.0)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve sandbox lifecycle and proxy genuine services inside native pVisor VMs.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Externally reachable host:port, without scheme/path. Required behind a proxy.
        #[arg(long)]
        public_endpoint: Option<String>,
        /// Private persistent state. Keep the absolute path short for Unix control sockets.
        #[arg(long, default_value = ".pvisor/daemon")]
        state: PathBuf,
        #[arg(long, env = "OPEN_SANDBOX_API_KEY", hide_env_values = true)]
        api_key: String,
        /// Trusted local prepared-image manifests and immutable rootfs trees.
        #[arg(long)]
        images_dir: PathBuf,
        /// Existing delegated cgroup v2 subtree for per-sandbox process-tree limits.
        #[arg(long)]
        cgroup_root: PathBuf,
        #[arg(long, default_value_t = 32)]
        max_sandboxes: usize,
        /// Total admitted CPU quota, in thousandths of one CPU. No implicit overcommit.
        #[arg(long, default_value_t = 4000)]
        cpu_millis: u64,
        /// Admitted hard limits include each sandbox's supervisor and VM process tree.
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
        memory_bytes: u64,
        #[arg(long, default_value_t = 86400)]
        max_timeout_seconds: u64,
    },
    /// Print the fixed protocol baseline, not a claim of full API support.
    Protocol,
    #[command(hide = true)]
    NativeSupervisor {
        #[arg(long)]
        sandbox_dir: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    // The re-executed VM runner enters namespaces before any Tokio threads exist.
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args = Args::parse();
    if matches!(args.command, Command::Protocol) {
        println!(
            "OpenSandbox {} ({})",
            daemon::OPENSANDBOX_VERSION,
            daemon::OPENSANDBOX_COMMIT
        );
        return Ok(());
    }
    // Each sandbox already has a native runner; avoid a host-sized Tokio worker
    // pool per supervisor on high-core-count machines.
    let mut runtime = if matches!(args.command, Command::NativeSupervisor { .. }) {
        tokio::runtime::Builder::new_current_thread()
    } else {
        tokio::runtime::Builder::new_multi_thread()
    };
    runtime.enable_all().build()?.block_on(run(args.command))
}

async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Protocol => unreachable!(),
        Command::NativeSupervisor { sandbox_dir } => {
            run_native_supervisor(&sandbox_dir).await?;
        }
        Command::Serve {
            listen,
            public_endpoint,
            state,
            api_key,
            images_dir,
            cgroup_root,
            max_sandboxes,
            cpu_millis,
            memory_bytes,
            max_timeout_seconds,
        } => {
            anyhow::ensure!(
                cfg!(all(target_os = "linux", target_arch = "x86_64")),
                "native daemon requires Linux x86-64, KVM and delegated cgroup v2"
            );
            anyhow::ensure!(
                (60..=365 * 24 * 60 * 60).contains(&max_timeout_seconds),
                "timeout limit must be between 60 seconds and one year"
            );
            anyhow::ensure!(
                (!listen.ip().is_unspecified() && listen.port() != 0) || public_endpoint.is_some(),
                "wildcard or ephemeral listen requires an explicit public endpoint"
            );
            let images_dir = images_dir.canonicalize()?;
            let cgroup_root = cgroup_root.canonicalize()?;
            let executable = std::env::current_exe()?.canonicalize()?;
            // Store::open creates and locks this directory before constructing the runtime.
            let runtime_state = if state.is_absolute() {
                state.clone()
            } else {
                std::env::current_dir()?.join(&state)
            };
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
                Ok(Arc::new(NativeRuntime::new(NativeRuntimeConfig {
                    state_dir: runtime_state.canonicalize()?,
                    owner,
                    cgroup_root,
                    images_dir,
                    executable,
                })?)
                    as Arc<dyn pvisor_daemon::runtime::Runtime>)
            })
            .await?;
            let maintenance = daemon.start_maintenance();
            let listener = tokio::net::TcpListener::bind(listen).await?;
            eprintln!(
                "pVisor daemon listening on {}; native VM prepared execd/egress images required",
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
    // Detached native supervisors and durable ownership survive daemon-only restart.
}

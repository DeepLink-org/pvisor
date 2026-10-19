mod cache_component;
use clap::{Parser, Subcommand};
use pvisor_daemon::{
    daemon::{self, Config, Daemon},
    runtime::{NativeRuntime, NativeRuntimeConfig, run_native_supervisor},
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "pVisor shared services: image cache, memory pool and VM sandboxes"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    CacheWorker {
        #[arg(long)]
        listen: String,
        #[arg(long)]
        image_store: Option<PathBuf>,
    },
    /// Run the selected shared-service components under one daemon lifecycle.
    Serve {
        /// Enable the cache component (disabled by default).
        #[arg(long)]
        cache: bool,
        /// Cache endpoint; defaults to PVISOR_CACHE_LOCATION / PVISOR_CACHE_SERVER.
        #[arg(long, requires = "cache")]
        cache_listen: Option<String>,
        /// Cache OCI staging directory; defaults to PVISOR_IMAGE_STORE.
        #[arg(long, requires = "cache")]
        cache_image_store: Option<PathBuf>,
        /// Enable the sandbox lifecycle API.
        #[arg(long, requires_all = ["images_dir", "cgroup_root"])]
        sandbox: bool,
        #[arg(long, default_value = "127.0.0.1:8080", requires = "sandbox")]
        listen: SocketAddr,
        /// Externally reachable host:port, without scheme/path. Required behind a proxy.
        #[arg(long, requires = "sandbox")]
        public_endpoint: Option<String>,
        /// Private persistent state. Keep the absolute path short for Unix control sockets.
        #[arg(long, default_value = ".pvisor/daemon", requires = "sandbox")]
        state: PathBuf,
        /// Sandbox API key; defaults to OPEN_SANDBOX_API_KEY.
        #[arg(long, requires = "sandbox")]
        api_key: Option<String>,
        /// Trusted local prepared-image manifests and immutable rootfs trees.
        #[arg(long, requires = "sandbox")]
        images_dir: Option<PathBuf>,
        /// Existing delegated cgroup v2 subtree for per-sandbox process-tree limits.
        #[arg(long, requires = "sandbox")]
        cgroup_root: Option<PathBuf>,
        #[arg(long, default_value_t = 32, requires = "sandbox")]
        max_sandboxes: usize,
        /// Total admitted CPU quota, in thousandths of one CPU. No implicit overcommit.
        #[arg(long, default_value_t = 4000, requires = "sandbox")]
        cpu_millis: u64,
        /// Admitted hard limits include each sandbox's supervisor and VM process tree.
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024, requires = "sandbox")]
        memory_bytes: u64,
        #[arg(long, default_value_t = 86400, requires = "sandbox")]
        max_timeout_seconds: u64,
        /// Enable the daemon-owned experimental shared cold-page pool.
        #[arg(long, requires = "sandbox")]
        memory_pool: bool,
        /// Physical page sharing or sealed compression plus content deduplication.
        #[arg(long, value_enum, requires = "memory_pool")]
        memory_pool_mode: Option<pvisor_daemon::memory_pool::PoolMode>,
    },
    /// Print the fixed protocol baseline, not a claim of full API support.
    Protocol,
    #[command(hide = true)]
    MemoryPool {
        #[arg(long)]
        directory: PathBuf,
    },
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
    if let Command::CacheWorker {
        listen,
        image_store,
    } = args.command
    {
        return cache_component::worker(listen, image_store);
    }
    if let Command::MemoryPool { directory } = &args.command {
        return pvisor_daemon::memory_pool::run(directory);
    }
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
        Command::Protocol | Command::MemoryPool { .. } | Command::CacheWorker { .. } => {
            unreachable!()
        }
        Command::NativeSupervisor { sandbox_dir } => {
            run_native_supervisor(&sandbox_dir).await?;
        }
        Command::Serve {
            cache,
            cache_listen,
            cache_image_store,
            sandbox,
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
            memory_pool,
            memory_pool_mode,
        } => {
            anyhow::ensure!(
                cache || sandbox,
                "select at least one service: --cache or --sandbox"
            );
            // Prepare sandbox resources before starting optional components.
            let sandbox_service = if sandbox {
                anyhow::ensure!(
                    cfg!(all(target_os = "linux", target_arch = "x86_64")),
                    "sandbox service requires Linux x86-64, KVM and delegated cgroup v2"
                );
                anyhow::ensure!(
                    (60..=365 * 24 * 60 * 60).contains(&max_timeout_seconds),
                    "timeout limit must be between 60 seconds and one year"
                );
                anyhow::ensure!(
                    (!listen.ip().is_unspecified() && listen.port() != 0)
                        || public_endpoint.is_some(),
                    "wildcard or ephemeral listen requires an explicit public endpoint"
                );
                let images_dir = images_dir
                    .ok_or_else(|| anyhow::anyhow!("sandbox requires --images-dir"))?
                    .canonicalize()?;
                let cgroup_root = cgroup_root
                    .ok_or_else(|| anyhow::anyhow!("sandbox requires --cgroup-root"))?
                    .canonicalize()?;
                let executable = std::env::current_exe()?.canonicalize()?;
                // Store::open creates and locks this directory before constructing the runtime.
                let runtime_state = if state.is_absolute() {
                    state.clone()
                } else {
                    std::env::current_dir()?.join(&state)
                };
                let config = Config {
                    state_dir: state,
                    api_key: api_key
                        .or_else(|| std::env::var("OPEN_SANDBOX_API_KEY").ok())
                        .ok_or_else(|| {
                            anyhow::anyhow!("sandbox requires --api-key or OPEN_SANDBOX_API_KEY")
                        })?,
                    public_endpoint: public_endpoint.unwrap_or_else(|| listen.to_string()),
                    max_sandboxes,
                    cpu_millis,
                    memory_bytes,
                    max_timeout_seconds,
                };
                let daemon = Daemon::open(config, move |owner| {
                    let runtime = NativeRuntime::new(NativeRuntimeConfig {
                        state_dir: runtime_state.canonicalize()?,
                        owner,
                        cgroup_root,
                        images_dir,
                        executable: executable.clone(),
                    })?;
                    let pool = if memory_pool {
                        Some(pvisor_daemon::memory_pool::ensure_service(
                            &runtime_state.canonicalize()?,
                            &executable,
                            pvisor_daemon::memory_pool::PoolConfig {
                                mode: memory_pool_mode.unwrap_or_default(),
                                ..Default::default()
                            },
                        )?)
                    } else {
                        None
                    };
                    Ok(Arc::new(runtime.with_memory_pool(pool))
                        as Arc<dyn pvisor_daemon::runtime::Runtime>)
                })
                .await?;
                let listener = tokio::net::TcpListener::bind(listen).await?;
                Some((daemon, listener))
            } else {
                None
            };
            let mut cache = if cache {
                Some(cache_component::start(cache_listen, cache_image_store).await?)
            } else {
                None
            };
            let maintenance = sandbox_service
                .as_ref()
                .map(|(daemon, _)| daemon.start_maintenance());
            let (api_stop, api_stopped) = tokio::sync::oneshot::channel::<()>();
            let api_enabled = sandbox_service.is_some();
            let api = async {
                if let Some((daemon, listener)) = sandbox_service {
                    eprintln!(
                        "pVisor sandbox service listening on {}",
                        listener.local_addr()?
                    );
                    axum::serve(listener, daemon::api::router(daemon))
                        .with_graceful_shutdown(async {
                            let _ = api_stopped.await;
                        })
                        .await?;
                    return Ok(());
                }
                std::future::pending::<anyhow::Result<()>>().await
            };
            tokio::pin!(api);
            let (result, api_finished) = tokio::select! {
                _ = shutdown() => (Ok(()), false),
                result = &mut api => (result.and_then(|_| Err(anyhow::anyhow!("sandbox service exited unexpectedly"))), true),
                result = cache_component::wait(&mut cache) => (result, false),
            };
            let _ = api_stop.send(());
            let drained = if api_enabled && !api_finished {
                tokio::time::timeout(std::time::Duration::from_secs(10), api)
                    .await
                    .map_err(|_| anyhow::anyhow!("sandbox service shutdown timed out"))
                    .and_then(|result| result)
            } else {
                Ok(())
            };
            if let Some(task) = maintenance {
                task.abort();
            }
            if let Some(mut child) = cache {
                cache_component::stop(&mut child).await?;
            }
            result?;
            drained?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_daemon::memory_pool::PoolMode;

    #[test]
    fn composite_service_selection_and_component_options() {
        assert!(matches!(
            Args::try_parse_from(["pvisor-daemon", "serve", "--cache"])
                .unwrap()
                .command,
            Command::Serve {
                cache: true,
                sandbox: false,
                images_dir: None,
                ..
            }
        ));
        assert!(Args::try_parse_from(["pvisor-daemon", "cache"]).is_err());
        assert!(Args::try_parse_from(["pvisor-daemon", "serve", "--sandbox"]).is_err());
        assert!(
            Args::try_parse_from([
                "pvisor-daemon",
                "serve",
                "--cache-listen",
                "unix:///tmp/cache.sock"
            ])
            .is_err()
        );
        assert!(
            Args::try_parse_from([
                "pvisor-daemon",
                "serve",
                "--cache",
                "--sandbox",
                "--api-key",
                "fixture-key",
                "--images-dir",
                "/images",
                "--cgroup-root",
                "/groups"
            ])
            .is_ok()
        );
    }

    #[test]
    fn sandbox_options_cannot_implicitly_select_or_configure_a_disabled_component() {
        for options in [
            vec!["--images-dir", "/images", "--cgroup-root", "/groups"],
            vec!["--api-key", "fixture-key"],
            vec!["--listen", "127.0.0.1:8081"],
            vec!["--public-endpoint", "example.test:8080"],
            vec!["--state", "/state"],
            vec!["--max-sandboxes", "2"],
            vec!["--cpu-millis", "1000"],
            vec!["--memory-bytes", "1073741824"],
            vec!["--max-timeout-seconds", "60"],
            vec!["--memory-pool"],
        ] {
            let mut args = vec!["pvisor-daemon", "serve", "--cache"];
            args.extend(options);
            assert!(Args::try_parse_from(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn compressed_pool_selection_requires_explicit_pool_enablement() {
        let base = [
            "pvisor-daemon",
            "serve",
            "--sandbox",
            "--api-key",
            "fixture-key",
            "--images-dir",
            "/images",
            "--cgroup-root",
            "/groups",
        ];
        let mut args = base.to_vec();
        args.extend(["--memory-pool-mode", "compressed"]);
        assert!(Args::try_parse_from(&args).is_err());
        args.push("--memory-pool");
        assert!(matches!(
            Args::try_parse_from(&args).unwrap().command,
            Command::Serve {
                memory_pool: true,
                memory_pool_mode: Some(PoolMode::Compressed),
                ..
            }
        ));
        assert!(matches!(
            Args::try_parse_from(base).unwrap().command,
            Command::Serve {
                memory_pool: false,
                memory_pool_mode: None,
                ..
            }
        ));
    }
}

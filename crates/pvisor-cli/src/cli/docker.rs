//! Docker command syntax; storage, image preparation and VM ownership live in pvisor.
use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};
use pvisor::docker::{self, Store};
use pvisor_guest::api::{
    GuestFrameRead, GuestFrameWrite, Reply as GuestReply, Request as GuestRequest,
    StdinFrame as GuestStdin,
};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

mod build;
mod builder;
mod compose;
mod network;
mod progress;
mod query;
mod transfer;
mod volume_cli;

impl DockerCommand {
    pub(super) fn failure_exit_code(&self) -> i32 {
        if matches!(
            self,
            DockerCommand::Run(_)
                | DockerCommand::Container {
                    command: ContainerCommand::Run(_)
                }
        ) {
            125
        } else {
            1
        }
    }
}

#[derive(Debug, Subcommand)]
pub(super) enum DockerCommand {
    /// Build a local Dockerfile using VM-isolated RUN instructions.
    Build(build::Build),
    /// Manage local build cache.
    Builder(builder::BuilderArgs),
    /// Manage image-based Compose projects without a Docker daemon.
    Compose(compose::ComposeArgs),
    /// Run an image in its own VM, or use @root to run host-installed tools (Linux x86_64).
    #[command(disable_help_flag = true)]
    Run(Box<Run>),
    /// Manage containers using Docker's grouped command spelling.
    Container {
        #[command(subcommand)]
        command: ContainerCommand,
    },
    /// Manage images using Docker's grouped command spelling.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Manage container networks.
    Network(network::NetworkArgs),
    /// Manage persistent local volumes.
    Volume(volume_cli::VolumeArgs),
    /// Download an OCI image or import a pinned lazy publication.
    Pull(transfer::Pull),
    /// Publish a local image to an explicit S3 or file destination.
    Push(transfer::Push),
    /// List locally pulled images.
    Images(ListImages),
    /// Remove one or more local image references.
    Rmi(RemoveImages),
    /// Execute a command inside a running container VM.
    Exec(Exec),
    /// List containers.
    Ps(List),
    /// Start one or more stopped containers, retaining their writable files.
    Start(Start),
    /// Stop one or more running containers.
    Stop(Stop),
    /// Remove one or more containers.
    Rm(Remove),
    /// Fetch container stdout and stderr.
    Logs(Logs),
    /// Stop and restart containers, retaining their files and volumes.
    Restart(Restart),
    /// Print the selected container generation's exit code.
    Wait(Select),
    /// Copy files between a host and a container.
    Cp(Copy),
    /// Inspect Session/container or image metadata; --files opens a task view.
    Inspect(Inspect),
    /// Signal the selected Session's main execution.
    Kill(Kill),
}

#[derive(Debug, Subcommand)]
pub(super) enum ContainerCommand {
    /// Fork an idle running VM, sharing one immutable RAM image.
    Branch {
        source: String,
        /// Allow each child to offload private RAM to its own sparse backing.
        #[arg(long)]
        offload: bool,
        /// Delegated cgroup v2 path per child, in name order, for pressure-managed offload.
        #[arg(long, value_name = "PATH", conflicts_with = "offload")]
        pressure_cgroup: Vec<PathBuf>,
        /// Physical page-sharing pool for post-fork private guest pages.
        #[arg(long, requires = "pressure_cgroup")]
        memory_pool: Option<PathBuf>,
        #[arg(long, required=true, num_args=1..)]
        names: Vec<String>,
    },
    /// Park a fork child and reclaim its RAM until an explicit resume.
    Offload {
        child: String,
    },
    /// Resume an offloaded fork child.
    Resume {
        child: String,
    },
    #[command(disable_help_flag = true)]
    Run(Box<Run>),
    Exec(Exec),
    #[command(alias = "list", alias = "ps")]
    Ls(List),
    Start(Start),
    Stop(Stop),
    #[command(alias = "remove")]
    Rm(Remove),
    Logs(Logs),
    Inspect(Inspect),
    Restart(Restart),
    Wait(Select),
    Cp(Copy),
    Kill(Kill),
}

#[derive(Debug, Subcommand)]
pub(super) enum ImageCommand {
    Build(Box<build::Build>),
    /// Download an OCI image or import a pinned lazy publication.
    Pull(transfer::Pull),
    /// Publish a local image to an explicit S3 or file destination.
    Push(transfer::Push),
    #[command(alias = "list")]
    Ls(ListImages),
    #[command(alias = "remove")]
    Rm(RemoveImages),
    /// Reclaim temporary work left by interrupted local image imports.
    PruneStaging,
    Inspect(Inspect),
}

impl DockerCommand {
    fn ungroup(self) -> Self {
        match self {
            Self::Container { command } => match command {
                command @ (ContainerCommand::Branch { .. }
                | ContainerCommand::Offload { .. }
                | ContainerCommand::Resume { .. }) => Self::Container { command },
                ContainerCommand::Run(args) => Self::Run(args),
                ContainerCommand::Exec(args) => Self::Exec(args),
                ContainerCommand::Ls(args) => Self::Ps(args),
                ContainerCommand::Start(args) => Self::Start(args),
                ContainerCommand::Stop(args) => Self::Stop(args),
                ContainerCommand::Rm(args) => Self::Rm(args),
                ContainerCommand::Logs(args) => Self::Logs(args),
                ContainerCommand::Restart(args) => Self::Restart(args),
                ContainerCommand::Wait(args) => Self::Wait(args),
                ContainerCommand::Cp(args) => Self::Cp(args),
                ContainerCommand::Kill(args) => Self::Kill(args),
                ContainerCommand::Inspect(args) => Self::Container {
                    command: ContainerCommand::Inspect(args),
                },
            },
            Self::Image { command } => match command {
                ImageCommand::Build(args) => Self::Build(*args),
                ImageCommand::Pull(args) => Self::Pull(args),
                ImageCommand::Push(args) => Self::Push(args),
                ImageCommand::Ls(args) => Self::Images(args),
                ImageCommand::Rm(args) => Self::Rmi(args),
                command @ (ImageCommand::Inspect(_) | ImageCommand::PruneStaging) => {
                    Self::Image { command }
                }
            },
            command => command,
        }
    }
}

#[derive(Debug, Args)]
pub(super) struct Run {
    /// Print help; -h is reserved for Docker's currently unsupported hostname option.
    #[arg(long = "help", action = clap::ArgAction::Help)]
    help: Option<bool>,
    /// Select the image platform. Execution currently requires the native platform.
    #[arg(long, value_parser = ["linux/amd64", "linux/arm64", "linux/arm64/v8"])]
    platform: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(short, long)]
    detach: bool,
    #[arg(long)]
    rm: bool,
    #[arg(short, long)]
    interactive: bool,
    #[arg(short = 't', long)]
    tty: bool,
    #[arg(short = 'e', long = "env", action = clap::ArgAction::Append)]
    env: Vec<String>,
    #[arg(long = "env-file")]
    env_files: Vec<PathBuf>,
    #[arg(short = 'w', long = "workdir")]
    cwd: Option<PathBuf>,
    #[arg(short = 'u', long)]
    user: Option<String>,
    #[arg(long)]
    entrypoint: Option<String>,
    /// Bind or volume: HOST:GUEST[:ro|rw|stage]; with @root, use PATH:/:stage for changeset storage.
    #[arg(short = 'v', long = "volume")]
    volumes: Vec<String>,
    #[arg(short = 'p', long = "publish")]
    ports: Vec<String>,
    /// Native vCPU count; fractional Docker CPU quotas are unsupported.
    #[arg(long, default_value_t = 2)]
    cpus: u8,
    #[arg(short = 'm', long, default_value = "512m", value_parser = memory)]
    memory: u32,
    /// Experimental shared pool: auto finds/starts the same-user daemon; a socket requires an existing pool.
    #[arg(long, value_name = "auto|SOCKET")]
    memory_pool: Option<PathBuf>,
    /// Enable cooperative daemon memory controls (Linux); does not enable automatic offload.
    #[arg(long, conflicts_with_all = ["memory_pool", "shared_memory_pool", "shared_kernel_base", "branchable"])]
    managed_memory: bool,
    /// Use the same-user shared memory pool, starting its daemon if needed (--memory-pool auto).
    #[arg(short = 'M', conflicts_with = "memory_pool")]
    shared_memory_pool: bool,
    /// Share boot-kernel RAM through a private directory (Apple Silicon; experimental).
    #[arg(long, value_name = "DIR")]
    shared_kernel_base: Option<PathBuf>,
    /// Network: bridge (image default), none (@root default), or a user-defined network (image only).
    #[arg(long, alias = "net")]
    network: Option<String>,
    /// Static IPv4 in a user-defined network's configured subnet.
    #[arg(long)]
    ip: Option<std::net::Ipv4Addr>,
    /// Metadata label KEY[=VALUE]; repeated keys use the last value.
    #[arg(short = 'l', long = "label")]
    labels: Vec<String>,
    /// no, always, unless-stopped, or on-failure[:MAX_RETRIES].
    #[arg(long, default_value = "no")]
    restart: docker::RestartPolicy,
    /// Override the image's Linux stop signal.
    #[arg(long)]
    stop_signal: Option<String>,
    /// Default stop grace in nonnegative seconds.
    #[arg(long)]
    stop_timeout: Option<u64>,
    /// Disable the image's default health check.
    #[arg(long)]
    no_healthcheck: bool,
    /// Permit a detached guest to fork into independent children.
    #[arg(long)]
    branchable: bool,
    /// Image or @root, followed by the optional command and its literal arguments.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, value_names = ["IMAGE", "COMMAND"])]
    invocation: Vec<String>,
}

impl Run {
    fn into_root_stage(self) -> anyhow::Result<super::stage::StageArgs> {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "@root requires Linux x86_64 host-root VM support (no fallback)"
        );
        let Self {
            help: _,
            platform,
            name,
            detach,
            rm,
            interactive,
            tty,
            env,
            env_files,
            cwd,
            user,
            entrypoint,
            volumes,
            ports,
            cpus,
            memory,
            memory_pool,
            managed_memory,
            shared_memory_pool,
            shared_kernel_base,
            network,
            ip,
            labels,
            restart,
            stop_signal,
            stop_timeout,
            no_healthcheck,
            branchable,
            mut invocation,
        } = self;
        for (flag, given) in [
            ("--platform", platform.is_some()),
            ("--detach", detach),
            ("--rm", rm),
            ("--user", user.is_some()),
            ("--entrypoint", entrypoint.is_some()),
            ("--publish", !ports.is_empty()),
            (
                "--memory-pool/-M",
                memory_pool.is_some() || shared_memory_pool,
            ),
            ("--managed-memory", managed_memory),
            ("--shared-kernel-base", shared_kernel_base.is_some()),
            ("--ip", ip.is_some()),
            ("--label", !labels.is_empty()),
            ("--restart", restart != docker::RestartPolicy::No),
            ("--stop-signal", stop_signal.is_some()),
            ("--stop-timeout", stop_timeout.is_some()),
            ("--no-healthcheck", no_healthcheck),
            ("--branchable", branchable),
        ] {
            ensure!(!given, "{flag} is unsupported with @root");
        }
        ensure!((1..=8).contains(&cpus), "@root --cpus must be 1 through 8");
        let network = network.unwrap_or_else(|| "none".into());
        ensure!(
            matches!(network.as_str(), "bridge" | "none"),
            "@root --network must be bridge or none"
        );
        let stage = match volumes.as_slice() {
            [] => None,
            [value] => {
                let path = value
                    .strip_suffix(":/:stage")
                    .filter(|path| !path.is_empty() && !path.contains(':'))
                    .context("@root only supports --volume PATH:/:stage for changeset storage")?;
                Some(PathBuf::from(path))
            }
            _ => bail!("@root accepts at most one --volume PATH:/:stage"),
        };
        let command = invocation.split_off(1);
        Ok(super::stage::StageArgs {
            interactive,
            tty,
            env,
            env_files,
            cwd,
            name,
            cpus,
            memory,
            network,
            stage,
            command,
        })
    }
}

#[derive(Debug, Args)]
pub(super) struct Select {
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct Start {
    /// Stream the new execution's output and return its exit status (one container).
    #[arg(short, long)]
    attach: bool,
    #[command(flatten)]
    select: Select,
}
#[derive(Debug, Args)]
pub(super) struct Stop {
    /// Seconds before force-stop; -1 waits indefinitely for graceful exit.
    #[arg(short = 't', long, allow_hyphen_values = true, value_parser = clap::value_parser!(i64).range(-1..))]
    timeout: Option<i64>,
    #[arg(short = 's', long)]
    signal: Option<String>,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct Remove {
    /// Finish an interrupted directory cleanup by full container ID.
    #[arg(long, conflicts_with_all = ["force", "volumes"])]
    retired: bool,
    #[arg(short, long)]
    force: bool,
    #[arg(short, long)]
    volumes: bool,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct RemoveImages {
    #[arg(short, long)]
    force: bool,
    #[arg(required = true, num_args = 1..)]
    images: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct List {
    #[arg(short, long)]
    all: bool,
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
    #[arg(short = 'f', long = "filter")]
    filters: Vec<String>,
    #[arg(long)]
    format: Option<String>,
}
#[derive(Debug, Args)]
pub(super) struct ListImages {
    /// Query the prepared-template catalog instead of ordinary images.
    #[arg(long)]
    images_dir: Option<PathBuf>,
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
    #[arg(short = 'f', long = "filter")]
    filters: Vec<String>,
    #[arg(long)]
    format: Option<String>,
    repository: Option<String>,
}
#[derive(Debug, Args)]
pub(super) struct Logs {
    #[arg(short, long)]
    follow: bool,
    container: String,
}
#[derive(Debug, Args)]
pub(super) struct Inspect {
    /// Query a prepared template without resolving or downloading it.
    #[arg(long, conflicts_with_all = ["files", "checkpoint", "command"])]
    images_dir: Option<PathBuf>,
    #[arg(short = 'f', long)]
    format: Option<String>,
    #[arg(required_unless_present_any = ["files", "checkpoint", "command"], num_args = 1..)]
    selectors: Vec<String>,
    /// Restrict metadata lookup to a resource kind.
    #[arg(long = "type", value_parser = ["container", "image"])]
    kind: Option<String>,
    /// Open a native task's read-only filesystem rather than metadata.
    #[arg(long, conflicts_with_all = ["format", "kind"])]
    files: bool,
    #[arg(long, conflicts_with_all = ["format", "kind"])]
    checkpoint: Option<String>,
    #[arg(
        long = "store",
        alias = "output-dir",
        short = 'o',
        default_value = ".pvisor/capture"
    )]
    output_dir: PathBuf,
    #[arg(last = true, allow_hyphen_values = true, conflicts_with_all = ["format", "kind"])]
    command: Vec<String>,
}

#[derive(Debug, Args)]
pub(super) struct Kill {
    #[arg(short = 's', long, default_value = "KILL")]
    signal: String,
    #[arg(required = true, num_args = 1..)]
    selectors: Vec<String>,
    /// Native task cancellation response.
    #[arg(long)]
    json: bool,
    #[arg(short = 'o', long, default_value = ".pvisor/capture")]
    output_dir: PathBuf,
}

#[derive(Debug, Args)]
pub(super) struct Restart {
    #[arg(short = 't', long, allow_hyphen_values = true, value_parser = clap::value_parser!(i64).range(-1..))]
    timeout: Option<i64>,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}

#[derive(Debug, Args)]
pub(super) struct Copy {
    source: String,
    destination: String,
}

#[derive(Debug, Args)]
pub(super) struct Exec {
    /// Interpret TARGET as a live Session selector (Linux Host sandbox only).
    #[arg(long)]
    session: bool,
    #[arg(short, long)]
    interactive: bool,
    #[arg(short = 't', long)]
    tty: bool,
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    #[arg(long = "env-file")]
    env_files: Vec<PathBuf>,
    #[arg(short = 'w', long = "workdir")]
    cwd: Option<PathBuf>,
    #[arg(short = 'u', long)]
    user: Option<String>,
    /// Container (or --session selector), executable and literal arguments.
    #[arg(required = true, num_args = 2.., trailing_var_arg = true, value_names = ["TARGET", "COMMAND"])]
    invocation: Vec<String>,
}

pub(super) use crate::container_args::environment;
use crate::container_args::{HOST_ROOT_SELECTOR, PsRow, batch, batch_results, ps_header, ps_row};

pub(super) fn memory(value: &str) -> Result<u32, String> {
    docker::parse_memory_limit(value).map_err(|error| error.to_string())
}

fn volume(value: &str, store: &Store) -> anyhow::Result<docker::Mount> {
    let parts: Vec<_> = value.split(':').collect();
    ensure!(
        (1..=3).contains(&parts.len()),
        "invalid volume specification"
    );
    ensure!(
        !(parts.len() == 2 && parts[1] == "stage"),
        "stage requires HOST:GUEST:stage, not an anonymous volume"
    );
    let (source, target, mode) = if parts.len() == 1 {
        (None, parts[0], "rw")
    } else {
        (
            Some(parts[0]),
            parts[1],
            parts.get(2).copied().unwrap_or("rw"),
        )
    };
    ensure!(
        matches!(mode, "ro" | "rw" | "stage"),
        "unsupported volume mode: {mode}"
    );
    let staged = mode == "stage";
    ensure!(
        !staged || source.is_some_and(|s| s.starts_with('/') || s.starts_with('.')),
        "stage requires an explicit bind source, not a named or anonymous volume"
    );
    let (source, managed) = match source {
        Some(source) if source.starts_with('/') || source.starts_with('.') => {
            let path = PathBuf::from(source)
                .canonicalize()
                .context("bind source must be an existing directory or ordinary file")?;
            (path, None)
        }
        Some(name) => (store.volume(name)?, Some(name.into())),
        None => {
            let name = uuid::Uuid::new_v4().simple().to_string();
            (store.volume(&name)?, Some(format!("anonymous:{name}")))
        }
    };
    Ok(docker::Mount {
        source,
        target: target.into(),
        read_only: mode == "ro",
        staged,
        volume: managed,
    })
}

fn port(value: &str) -> anyhow::Result<docker::Port> {
    docker::parse_port_mapping(value)
}

pub(super) fn run(command: DockerCommand) -> anyhow::Result<i32> {
    let command = match command.ungroup() {
        DockerCommand::Run(args) if args.invocation[0] == HOST_ROOT_SELECTOR => {
            let run = Box::new(args.into_root_stage()?.into_run()?);
            return super::host_service::call(super::host::SessionCommand::Run(run));
        }
        command => command,
    };
    if let DockerCommand::Exec(args) = &command
        && args.session
    {
        anyhow::ensure!(
            args.env.is_empty()
                && args.env_files.is_empty()
                && args.cwd.is_none()
                && args.user.is_none(),
            "Session exec inherits cwd, environment and identity; overrides are unsupported"
        );
        let _terminal = Terminal::new(args.tty, args.interactive)?;
        return super::session_exec::run(
            std::path::Path::new(&args.invocation[0]),
            &args.invocation[1..],
            args.interactive,
            args.tty,
        );
    }
    if let DockerCommand::Build(args) = command {
        return build::run(args);
    }
    if let DockerCommand::Network(args) = command {
        return network::run(args);
    }
    if let DockerCommand::Builder(args) = command {
        return builder::run(args);
    }
    if let DockerCommand::Compose(args) = command {
        return compose::run(args);
    }
    if let DockerCommand::Volume(args) = command {
        return volume_cli::run(args);
    }
    if let DockerCommand::Push(args) = command {
        return transfer::push(args);
    }
    if let DockerCommand::Pull(args) = command {
        return transfer::pull(args);
    }
    if let DockerCommand::Images(args) = command {
        return query::images(args);
    }
    if let DockerCommand::Container { command } = &command {
        match command {
            ContainerCommand::Branch {
                source,
                names,
                offload,
                pressure_cgroup,
                memory_pool,
            } => {
                let store = Store::discover()?;
                let children = if pressure_cgroup.is_empty() {
                    store.fork_running_with_offload(source, names, *offload)?
                } else {
                    store.fork_running_with_pressure(
                        source,
                        names,
                        pressure_cgroup,
                        memory_pool.as_deref(),
                    )?
                };
                for child in children {
                    println!("{}", child.name);
                }
                return Ok(0);
            }
            ContainerCommand::Offload { child } | ContainerCommand::Resume { child } => {
                Store::discover()?.set_branch_offloaded(
                    child,
                    matches!(command, ContainerCommand::Offload { .. }),
                )?;
                return Ok(0);
            }
            _ => {}
        }
    }
    if let DockerCommand::Image {
        command: ImageCommand::PruneStaging,
    } = command
    {
        let removed = Store::discover()?.prune_image_staging()?;
        println!("Removed {removed} interrupted image import directories.");
        return Ok(0);
    }
    if matches!(
        command,
        DockerCommand::Container { .. } | DockerCommand::Image { .. } | DockerCommand::Inspect(_)
    ) {
        return query::inspect(command);
    }
    if let DockerCommand::Kill(args) = command {
        return query::kill(args);
    }
    query::validate(&command)?;
    let store = Store::discover()?;
    match command {
        DockerCommand::Build(_)
        | DockerCommand::Builder(_)
        | DockerCommand::Compose(_)
        | DockerCommand::Volume(_)
        | DockerCommand::Network(_)
        | DockerCommand::Container { .. }
        | DockerCommand::Image { .. } => {
            unreachable!()
        }
        DockerCommand::Inspect(_)
        | DockerCommand::Kill(_)
        | DockerCommand::Pull(_)
        | DockerCommand::Push(_)
        | DockerCommand::Images(_) => unreachable!(),
        DockerCommand::Rmi(args) => {
            return batch(&args.images, |selector| {
                for tag in store.rmi(selector, args.force)? {
                    println!("Untagged: {tag}");
                }
                Ok(())
            });
        }
        DockerCommand::Ps(args) => {
            return query::containers(&store, args);
        }
        DockerCommand::Run(args) => {
            ensure!(
                !(args.tty && args.interactive && !args.detach) || unsafe { libc::isatty(0) } == 1,
                "the input device is not a TTY"
            );
            let network = args.network.as_deref().unwrap_or("bridge");
            ensure!(
                !network.starts_with("container:") && network != "host",
                "network mode is unsupported: {network}"
            );
            let selected_network = store.inspect_network(network)?;
            ensure!(
                !selected_network.builtin || args.ip.is_none(),
                "--ip requires a user-defined network"
            );
            let (env, unset_env) = environment(&args.env, &args.env_files)?;
            let config = docker::Create {
                name: args.name,
                image: args.invocation[0].clone(),
                platform: args.platform,
                command: args.invocation[1..].to_vec(),
                entrypoint: args.entrypoint,
                env,
                unset_env,
                cwd: args.cwd,
                user: args.user,
                mounts: args
                    .volumes
                    .iter()
                    .map(|v| volume(v, &store))
                    .collect::<anyhow::Result<_>>()?,
                ports: args
                    .ports
                    .iter()
                    .map(|p| port(p))
                    .collect::<anyhow::Result<_>>()?,
                cpus: args.cpus,
                memory_mib: args.memory,
                memory_pool: args
                    .memory_pool
                    .or_else(|| args.shared_memory_pool.then(|| PathBuf::from("auto")))
                    .map(super::daemon::resolve_pool)
                    .transpose()?,
                managed_memory: args.managed_memory,
                shared_kernel_base: args.shared_kernel_base,
                auto_remove: args.rm,
                interactive: args.interactive,
                tty: args.tty.then(terminal_size),
                network: selected_network.name != "none",
                network_name: Some(selected_network.name),
                ipv4_address: args.ip,
                peers: None,
                labels: query::labels(&args.labels)?,
                restart: args.restart,
                stop_signal: args
                    .stop_signal
                    .as_deref()
                    .map(pvisor_guest::api::GuestSignal::parse_signal)
                    .transpose()?,
                stop_timeout_ms: args
                    .stop_timeout
                    .unwrap_or(10)
                    .checked_mul(1000)
                    .context("stop timeout overflow")?,
                healthcheck: None,
                no_healthcheck: args.no_healthcheck,
                branchable: args.branchable,
            };
            let instance = {
                let mut display = progress::PullDisplay::new(&config.image);
                #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                let create = Store::create_for_run;
                #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
                let create = Store::create_with_progress;
                create(&store, config, &mut |event| display.update(event))?
            };
            let output = store.output(&instance)?;
            let started = store.start(&instance.id)?;
            let _terminal = Terminal::new(args.tty && !args.detach, args.interactive)?;
            let _signals = if args.detach {
                None
            } else {
                Some(Signals::forward(store.clone(), started.clone())?)
            };
            if args.interactive && !args.detach && started.state != "exited" {
                forward_guest_stdin(&store, &started)?;
            }
            if args.detach {
                println!("{}", instance.id);
            } else {
                return output.follow(&started.generation, &mut io::stdout(), &mut io::stderr());
            }
        }
        DockerCommand::Start(args) => {
            if args.attach {
                let (started, output) = store.start_attached(&args.select.containers[0])?;
                let _signals = if started.state == "exited" {
                    None
                } else {
                    Some(Signals::forward(store, started.clone())?)
                };
                return output.follow(&started.generation, &mut io::stdout(), &mut io::stderr());
            }
            return batch(&args.select.containers, |selector| {
                store.start(selector)?;
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Stop(args) => {
            return batch(&args.containers, |selector| {
                let instance = store.resolve(selector)?;
                store.stop(
                    &instance.id,
                    args.timeout
                        .map(stop_duration)
                        .unwrap_or(Duration::from_millis(instance.config.stop_timeout_ms)),
                    args.signal.as_deref(),
                )?;
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Restart(args) => {
            return batch(&args.containers, |selector| {
                store.restart(selector, args.timeout.map(stop_duration))?;
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Wait(args) => {
            return batch_results(store.wait_each(&args.containers)?.map(|result| {
                println!("{}", result?);
                Ok(())
            }));
        }
        DockerCommand::Cp(args) => {
            let source = query::copy_endpoint(&args.source)?;
            let destination = query::copy_endpoint(&args.destination)?;
            match (source, destination) {
                (Some((container, path)), None) => {
                    store.copy_from(&container, &path, PathBuf::from(args.destination).as_path())?
                }
                (None, Some((container, path))) => {
                    store.copy_to(PathBuf::from(args.source).as_path(), &container, &path)?
                }
                _ => bail!("cp requires exactly one CONTAINER:PATH endpoint"),
            }
        }
        DockerCommand::Rm(args) => {
            return batch(&args.containers, |selector| {
                if args.retired {
                    store.remove_retired(selector)?;
                } else if args.force {
                    store.force_remove(selector, args.volumes)?;
                } else {
                    store.remove(selector, args.volumes)?;
                }
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Logs(args) => {
            store.logs(
                &args.container,
                args.follow,
                &mut io::stdout(),
                &mut io::stderr(),
            )?;
        }
        DockerCommand::Exec(args) => {
            let _terminal = Terminal::new(args.tty, args.interactive)?;
            let instance = store.resolve(&args.invocation[0])?;
            let mut stream = store.connect(&instance)?;
            let (mut env, unset_env) = environment(&args.env, &args.env_files)?;
            if args.tty
                && !instance.env.contains_key("TERM")
                && !unset_env.iter().any(|key| key == "TERM")
            {
                env.entry("TERM".into()).or_insert_with(|| "xterm".into());
            }
            GuestFrameWrite::write_frame(
                &mut stream,
                &GuestRequest::Exec {
                    argv: args.invocation[1..].to_vec(),
                    env,
                    unset_env,
                    cwd: args.cwd,
                    user: args.user,
                    tty: args.tty.then(terminal_size),
                },
            )?;
            let first: GuestReply = GuestFrameRead::read_frame(&mut stream)?;
            match first {
                GuestReply::Ready => {}
                GuestReply::Error { message } => {
                    eprintln!("{message}");
                    if let Ok(GuestReply::Exit { code }) = GuestFrameRead::read_frame(&mut stream) {
                        return Ok(code);
                    }
                    return Ok(125);
                }
                _ => bail!("unexpected guest response"),
            }
            stream.set_read_timeout(None)?;
            stream.set_write_timeout(None)?;
            if args.interactive {
                let mut input = stream.try_clone()?;
                std::thread::spawn(move || {
                    let _ = send_stdin(&mut input);
                });
            } else {
                GuestFrameWrite::write_frame(&mut stream, &GuestStdin::Eof {})?;
            }
            loop {
                match GuestFrameRead::read_frame(&mut stream)? {
                    GuestReply::Output { stream: 1, bytes } => {
                        io::stdout().write_all(&bytes)?;
                        io::stdout().flush()?;
                    }
                    GuestReply::Output { stream: 2, bytes } => {
                        io::stderr().write_all(&bytes)?;
                        io::stderr().flush()?;
                    }
                    GuestReply::Exit { code } => return Ok(code),
                    GuestReply::Error { message } => bail!("{message}"),
                    _ => bail!("invalid guest output channel"),
                }
            }
        }
    }
    Ok(0)
}

struct Signals(libc::sigset_t);
impl Signals {
    fn forward(store: Store, instance: docker::Instance) -> anyhow::Result<Self> {
        let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut signals);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                libc::sigaddset(&mut signals, signal);
            }
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, &mut previous);
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error).into());
            }
        }
        std::thread::spawn(move || {
            loop {
                let mut signal = 0;
                if unsafe { libc::sigwait(&signals, &mut signal) } != 0 {
                    break;
                }
                let result = (|| -> anyhow::Result<()> {
                    let mut stream = store.connect(&instance)?;
                    GuestFrameWrite::write_frame(&mut stream, &GuestRequest::Stop { signal })?;
                    ensure!(
                        matches!(
                            GuestFrameRead::read_frame::<GuestReply>(&mut stream)?,
                            GuestReply::Ready
                        ),
                        "guest rejected signal"
                    );
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("Could not forward signal to container: {error:#}");
                }
            }
        });
        Ok(Self(previous))
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
        }
    }
}

fn forward_guest_stdin(store: &Store, started: &docker::Instance) -> anyhow::Result<()> {
    let attach = || -> anyhow::Result<_> {
        let mut input = store.connect(started)?;
        GuestFrameWrite::write_frame(&mut input, &GuestRequest::Input {})?;
        match GuestFrameRead::read_frame::<GuestReply>(&mut input)? {
            GuestReply::Ready => {}
            GuestReply::Error { message } | GuestReply::Failure { message, .. } => {
                bail!("guest stdin unavailable: {message}");
            }
            _ => bail!("guest stdin unavailable"),
        }
        input.set_read_timeout(None)?;
        input.set_write_timeout(None)?;
        Ok(input)
    };
    let mut input = match attach() {
        Ok(attachment) => attachment,
        Err(error) => {
            let disconnected = error.downcast_ref::<io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::NotConnected
                        | io::ErrorKind::NotFound
                        | io::ErrorKind::UnexpectedEof
                )
            });
            let completed = if disconnected {
                store.wait_for_completion_after_disconnect(started)?
            } else {
                store.has_completed(started)?
            };
            if completed {
                return Ok(());
            }
            return Err(error);
        }
    };
    std::thread::spawn(move || {
        let _ = send_stdin(&mut input);
        let _ = GuestFrameRead::read_frame::<GuestReply>(&mut input);
    });
    Ok(())
}

fn send_stdin(stream: &mut std::os::unix::net::UnixStream) -> anyhow::Result<()> {
    let mut buffer = [0; 16384];
    loop {
        let size = io::stdin().read(&mut buffer)?;
        if size == 0 {
            GuestFrameWrite::write_frame(stream, &GuestStdin::Eof {})?;
            return Ok(());
        }
        GuestFrameWrite::write_frame(
            stream,
            &GuestStdin::Data {
                bytes: buffer[..size].to_vec(),
            },
        )?;
    }
}

fn terminal_size() -> [u16; 2] {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } == 0
        && size.ws_row > 0
        && size.ws_col > 0
    {
        [size.ws_row, size.ws_col]
    } else {
        [24, 80]
    }
}

struct Terminal(Option<libc::termios>);
impl Terminal {
    fn new(tty: bool, interactive: bool) -> anyhow::Result<Self> {
        if !tty || !interactive {
            return Ok(Self(None));
        }
        ensure!(
            unsafe { libc::isatty(0) } == 1,
            "the input device is not a TTY"
        );
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::tcgetattr(0, &mut saved) } == 0,
            "cannot read terminal attributes"
        );
        let mut raw = saved;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        ensure!(
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } == 0,
            "cannot configure terminal"
        );
        Ok(Self(Some(saved)))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, saved);
            }
        }
    }
}

fn split_tag(reference: &str) -> (&str, &str) {
    match reference.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository, tag),
        _ => (reference, "latest"),
    }
}

fn stop_duration(seconds: i64) -> Duration {
    if seconds == -1 {
        Duration::MAX
    } else {
        Duration::from_secs(seconds as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        command: DockerCommand,
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn root_run_preserves_launch_options_and_literal_workload() {
        for (network_option, network) in [
            (None, "none"),
            (Some("--network=none"), "none"),
            (Some("--net=bridge"), "bridge"),
        ] {
            let argv = ["pvisor", "container", "run"]
                .into_iter()
                .chain(network_option)
                .chain([
                    "-it",
                    "--name=trial",
                    "-e",
                    "A=value",
                    "--env-file=trial.env",
                    "--workdir=/work",
                    "--cpus=8",
                    "--memory=1g",
                    "--volume=../my changes:/:stage",
                    "@root",
                    "/bin/echo",
                    "--rm",
                    "--help",
                    "--network",
                    "host",
                ]);
            let DockerCommand::Run(args) = Cli::try_parse_from(argv).unwrap().command.ungroup()
            else {
                panic!()
            };
            let stage = args.into_root_stage().unwrap();
            assert!(stage.interactive && stage.tty);
            assert_eq!(stage.name.as_deref(), Some("trial"));
            assert_eq!(stage.env, ["A=value"]);
            assert_eq!(stage.env_files, [PathBuf::from("trial.env")]);
            assert_eq!(stage.cwd, Some(PathBuf::from("/work")));
            assert_eq!(stage.cpus, 8);
            assert_eq!(stage.memory, 1024);
            assert_eq!(stage.network, network, "{network_option:?}");
            assert_eq!(stage.stage, Some(PathBuf::from("../my changes")));
            assert_eq!(
                stage.command,
                ["/bin/echo", "--rm", "--help", "--network", "host"]
            );
        }
    }

    #[test]
    fn managed_memory_is_explicit_and_conflicts_with_pager() {
        use clap::Parser;
        let parsed = crate::cli::Cli::try_parse_from([
            "pvisor",
            "run",
            "--managed-memory",
            "alpine",
            "sleep",
            "1",
        ]);
        assert!(parsed.is_ok());
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "run",
                "--branchable",
                "--managed-memory",
                "alpine"
            ])
            .is_err()
        );
        assert!(
            crate::cli::Cli::try_parse_from(["pvisor", "run", "--managed-memory", "-M", "alpine"])
                .is_err()
        );
    }

    #[test]
    fn grouped_workload_arguments_and_exec_env_files_use_existing_contracts() {
        let parsed = Cli::try_parse_from([
            "pvisor",
            "container",
            "run",
            "alpine",
            "echo",
            "-h",
            "--help",
        ])
        .unwrap();
        let DockerCommand::Run(args) = parsed.command.ungroup() else {
            panic!()
        };
        assert_eq!(args.invocation, ["alpine", "echo", "-h", "--help"]);

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("env");
        std::fs::write(&file, "FIRST=file\nSECOND=kept\n").unwrap();
        for prefix in [vec!["pvisor", "exec"], vec!["pvisor", "container", "exec"]] {
            let mut argv = prefix;
            argv.extend([
                "--env-file",
                file.to_str().unwrap(),
                "-e",
                "FIRST=explicit",
                "instance",
                "echo",
                "--env-file",
                "guest-only",
            ]);
            let parsed = Cli::try_parse_from(argv).unwrap();
            let DockerCommand::Exec(args) = parsed.command.ungroup() else {
                panic!()
            };
            let (env, unset) = environment(&args.env, &args.env_files).unwrap();
            assert_eq!(env["FIRST"], "explicit");
            assert_eq!(env["SECOND"], "kept");
            assert!(unset.is_empty());
            assert_eq!(
                args.invocation,
                ["instance", "echo", "--env-file", "guest-only"]
            );
        }
    }

    #[test]
    fn stop_timeout_accepts_docker_infinite_wait_and_rejects_other_negative_values() {
        for (value, expected) in [("-1", -1), ("0", 0), ("10", 10)] {
            let parsed =
                Cli::try_parse_from(["pvisor", "stop", "--timeout", value, "container"]).unwrap();
            let DockerCommand::Stop(args) = parsed.command else {
                panic!()
            };
            assert_eq!(args.timeout, Some(expected));
            assert_eq!(args.containers, ["container"]);
        }
        assert!(Cli::try_parse_from(["pvisor", "stop", "-t=-2", "container"]).is_err());
        assert!(Cli::try_parse_from(["pvisor", "stop", "--timeout=-2", "container"]).is_err());
    }
    #[test]
    fn shared_pool_shortcut_preserves_image_and_guest_arguments() {
        let parsed =
            Cli::try_parse_from(["pvisor", "run", "-M", "-m", "256m", "alpine", "echo", "-M"])
                .unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert!(args.shared_memory_pool);
        assert!(args.memory_pool.is_none());
        assert_eq!(args.memory, 256);
        assert_eq!(args.invocation, ["alpine", "echo", "-M"]);
        assert!(
            Cli::try_parse_from(["pvisor", "run", "-M", "--memory-pool", "auto", "alpine"])
                .is_err()
        );
    }
    #[test]
    fn docker_network_and_ip_options_replace_custom_peer_flags() {
        let parsed = Cli::try_parse_from([
            "pvisor",
            "run",
            "--network",
            "dev",
            "--ip",
            "172.18.0.11",
            "alpine",
        ])
        .unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.network.as_deref(), Some("dev"));
        assert_eq!(args.ip, Some("172.18.0.11".parse().unwrap()));
        let parsed = Cli::try_parse_from(["pvisor", "run", "--net=dev", "alpine"]).unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.network.as_deref(), Some("dev"));
        assert!(args.ip.is_none());
        assert!(Cli::try_parse_from(["pvisor", "run", "--peer-network", "dev", "alpine"]).is_err());
        assert!(
            Cli::try_parse_from(["pvisor", "run", "--peer-address", "192.0.2.11", "alpine"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "network",
                "create",
                "--subnet",
                "172.20.0.0/24",
                "dev"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["pvisor", "network", "connect", "dev", "container"]).is_err());
    }

    #[test]
    fn pull_credentials_require_a_password_source_bound_to_the_username() {
        let parsed = Cli::try_parse_from([
            "pvisor",
            "pull",
            "--username",
            "deploy",
            "--password-stdin",
            "harbor.internal/task:1",
        ])
        .unwrap();
        let DockerCommand::Pull(transfer::Pull {
            username,
            password_stdin,
            image,
            ..
        }) = parsed.command
        else {
            panic!()
        };
        assert_eq!(username.as_deref(), Some("deploy"));
        assert!(password_stdin);
        assert_eq!(image, "harbor.internal/task:1");
        // A password without its username, or both password sources at once.
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "pull",
                "--password",
                "p",
                "harbor.internal/task:1"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "pull",
                "--username",
                "deploy",
                "--password",
                "p",
                "--password-stdin",
                "harbor.internal/task:1"
            ])
            .is_err()
        );
        for source in [
            ["--from", "https://mirror.example/image"],
            ["--images-dir", "catalog"],
        ] {
            assert!(
                Cli::try_parse_from([
                    "pvisor",
                    "image",
                    "pull",
                    "--username",
                    "deploy",
                    "--password-stdin",
                    source[0],
                    source[1],
                    "harbor.internal/task:1",
                ])
                .is_err()
            );
        }
        // Both grouped spellings route to the same command after ungrouping.
        let grouped = Cli::try_parse_from([
            "pvisor",
            "image",
            "pull",
            "--username",
            "deploy",
            "--password",
            "p",
            "harbor.internal/task:1",
        ])
        .unwrap()
        .command
        .ungroup();
        let DockerCommand::Pull(transfer::Pull {
            username: grouped_user,
            image: grouped_image,
            ..
        }) = grouped
        else {
            panic!()
        };
        assert_eq!(grouped_user.as_deref(), Some("deploy"));
        assert_eq!(grouped_image, "harbor.internal/task:1");
    }

    #[test]
    fn docker_platform_options_precede_the_image_and_preserve_guest_flags() {
        let parsed =
            Cli::try_parse_from(["pvisor", "pull", "--platform=linux/amd64", "alpine"]).unwrap();
        let DockerCommand::Pull(transfer::Pull {
            platform,
            image,
            username,
            ..
        }) = parsed.command
        else {
            panic!()
        };
        assert_eq!(platform.as_deref(), Some("linux/amd64"));
        assert_eq!(image, "alpine");
        assert!(username.is_none());
        let parsed = Cli::try_parse_from([
            "pvisor",
            "run",
            "--platform",
            "linux/arm64/v8",
            "alpine",
            "echo",
            "--platform=guest",
        ])
        .unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.platform.as_deref(), Some("linux/arm64/v8"));
        assert_eq!(args.invocation, ["alpine", "echo", "--platform=guest"]);
        for command in ["pull", "run"] {
            assert!(
                Cli::try_parse_from(["pvisor", command, "--platform=windows/amd64", "alpine",])
                    .is_err()
            );
        }
    }
    #[test]
    fn command_names_and_guest_arguments_have_separate_boundaries() {
        let parsed = Cli::try_parse_from([
            "pvisor",
            "run",
            "--name",
            "test",
            "alpine",
            "sh",
            "-c",
            "echo --privileged",
        ])
        .unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.invocation, ["alpine", "sh", "-c", "echo --privileged"]);
        assert!(Cli::try_parse_from(["pvisor", "run", "--privileged", "alpine"]).is_err());
        let parsed = Cli::try_parse_from(["pvisor", "run", "alpine", "--help"]).unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.invocation, ["alpine", "--help"]);
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "build", "."])
                .unwrap()
                .command,
            DockerCommand::Build(_)
        ));
        assert!(Cli::try_parse_from(["pvisor", "build", "--privileged", "."]).is_err());
        for command in [
            "run", "build", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm", "logs",
        ] {
            let error = Cli::try_parse_from(["pvisor", command, "--help"]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        }
    }
    #[test]
    fn docker_environment_files_and_explicit_unsets_preserve_precedence() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("env");
        let missing = format!("PVISOR_DOCKER_UNSET_{}", uuid::Uuid::new_v4().simple());
        std::fs::write(
            &file,
            format!("# comment\nVALUE=file\n{missing}=from-file\n"),
        )
        .unwrap();
        let (env, unset) =
            environment(&["VALUE=cli value".into(), missing.clone()], &[file]).unwrap();
        assert_eq!(env.get("VALUE").unwrap(), "cli value");
        assert!(!env.contains_key(&missing));
        assert_eq!(unset, [missing]);
    }

    #[test]
    fn staged_volumes_are_explicit_binds_and_never_allocate_managed_volumes() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("store")).unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        for mode in ["stage", "ro", "rw"] {
            let mount = volume(&format!("{}:/workspace:{mode}", source.display()), &store).unwrap();
            assert_eq!(mount.source, source.canonicalize().unwrap());
            assert_eq!(mount.target, PathBuf::from("/workspace"));
            assert_eq!(mount.read_only, mode == "ro");
            assert_eq!(mount.staged, mode == "stage");
            assert!(mount.volume.is_none());
        }
        for value in [
            "named:/workspace:stage",
            ":/workspace:stage",
            "/workspace:stage",
            "named:/workspace:ro,stage",
            "named:/workspace:stage,rw",
        ] {
            assert!(volume(value, &store).is_err(), "{value}");
        }
        assert_eq!(
            std::fs::read_dir(root.path().join("store/volumes"))
                .unwrap()
                .count(),
            0
        );
        let named = volume("named:/workspace:ro", &store).unwrap();
        assert_eq!(named.volume.as_deref(), Some("named"));
        assert!(named.read_only && !named.staged);
        let anonymous = volume("/workspace", &store).unwrap();
        assert!(anonymous.volume.unwrap().starts_with("anonymous:"));
        assert!(!anonymous.staged);
    }

    #[test]
    fn resource_and_port_values_are_validated_before_startup() {
        assert_eq!(memory("512m").unwrap(), 512);
        assert!(memory("0").is_err());
        assert!(memory("18446744073709551615g").is_err());
        let publish = port("127.0.0.1:8080:80/tcp").unwrap();
        assert_eq!((publish.host, publish.guest), (8080, 80));
        assert_eq!(port("80").unwrap().host, 0);
        assert!(port("53:53/udp").is_err());
        assert!(port("1:0").is_err());
    }
}

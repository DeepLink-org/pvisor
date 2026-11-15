//! Compose syntax and rendering; project ownership belongs to the runtime.
use super::*;
use pvisor::docker::compose::{BuildPolicy, BuildSettings, Project, RecreatePolicy, UpOptions};

#[derive(Debug, Args)]
pub(in crate::cli) struct ComposeArgs {
    /// One Compose YAML/JSON file; merging multiple files is unsupported.
    #[arg(short, long)]
    file: Option<PathBuf>,
    /// Override the project name (otherwise env, top-level name, then directory).
    #[arg(short = 'p', long)]
    project_name: Option<String>,
    #[command(subcommand)]
    command: ComposeCommand,
}

#[derive(Debug, Subcommand)]
enum ComposeCommand {
    /// Validate and print resolved configuration (may contain secrets).
    Config {
        #[arg(short, long)]
        quiet: bool,
        #[arg(long)]
        services: bool,
    },
    /// Pull selected service images, without creating containers.
    Pull(Services),
    /// Build selected local service images using the isolated builder.
    Build {
        #[arg(long)]
        no_cache: bool,
        #[arg(long)]
        pull: bool,
        services: Vec<String>,
    },
    /// Create/start services and their dependencies in separate VMs.
    Up {
        /// This initial profile requires detached mode.
        #[arg(short, long, required_unless_present = "wait")]
        detach: bool,
        /// Wait until services are running/healthy; implies detached mode.
        #[arg(long)]
        wait: bool,
        /// Bound dependency/readiness waits in seconds (VM startup has its own bound).
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        wait_timeout: u64,
        #[arg(long, conflicts_with = "no_build")]
        build: bool,
        #[arg(long)]
        no_build: bool,
        #[arg(long, conflicts_with = "no_recreate")]
        force_recreate: bool,
        #[arg(long)]
        no_recreate: bool,
        #[arg(long)]
        no_deps: bool,
        #[arg(short = 't', long, default_value_t = 10)]
        timeout: u64,
        services: Vec<String>,
    },
    /// List project containers, including stopped containers with --all.
    Ps {
        #[arg(short, long)]
        all: bool,
        #[arg(short, long)]
        quiet: bool,
        services: Vec<String>,
    },
    /// Fetch service output, prefixed by service name.
    Logs {
        #[arg(short, long)]
        follow: bool,
        services: Vec<String>,
    },
    /// Run a command in a service container (interactive TTY by default).
    Exec {
        /// Disable pseudo-TTY allocation, e.g. in scripts.
        #[arg(short = 'T', long)]
        no_tty: bool,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        interactive: bool,
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        #[arg(short = 'w', long = "workdir")]
        cwd: Option<PathBuf>,
        #[arg(short = 'u', long)]
        user: Option<String>,
        #[arg(required = true, num_args = 2.., trailing_var_arg = true, value_names = ["SERVICE", "COMMAND"])]
        invocation: Vec<String>,
    },
    /// Start existing selected containers without recreating them.
    Start(Services),
    /// Stop selected services, retaining their writable files.
    Stop {
        #[arg(short = 't', long, default_value_t = 10)]
        timeout: u64,
        services: Vec<String>,
    },
    /// Stop/remove project containers and their owned default network.
    Down {
        #[arg(short = 't', long, default_value_t = 10)]
        timeout: u64,
        #[arg(long)]
        remove_orphans: bool,
        /// Explicitly remove owned named volumes and container anonymous volumes.
        #[arg(short, long)]
        volumes: bool,
    },
}

#[derive(Debug, Args)]
struct Services {
    services: Vec<String>,
}

pub(super) fn run(args: ComposeArgs) -> anyhow::Result<i32> {
    let project = Project::load(args.file.as_deref(), args.project_name.as_deref())?;
    // Configuration and selection failures must not initialize Docker storage.
    let selection = match &args.command {
        ComposeCommand::Pull(args) | ComposeCommand::Start(args) => &args.services[..],
        ComposeCommand::Up { services, .. }
        | ComposeCommand::Build { services, .. }
        | ComposeCommand::Ps { services, .. }
        | ComposeCommand::Logs { services, .. }
        | ComposeCommand::Stop { services, .. } => services,
        ComposeCommand::Exec { invocation, .. } => &invocation[..1],
        _ => &[],
    };
    project.order(selection, false)?;
    if let ComposeCommand::Config { quiet, services } = args.command {
        if !quiet {
            if services {
                for service in project.order(&[], false)? {
                    println!("{service}");
                }
            } else {
                print!("{}", project.config()?);
            }
        }
        return Ok(0);
    }
    let store = Store::discover()?;
    match args.command {
        ComposeCommand::Config { .. } => unreachable!(),
        ComposeCommand::Pull(args) => {
            let mut display = progress::PullDisplay::new(project.name());
            project.pull(&store, &args.services, &mut |event| display.update(event))?;
        }
        ComposeCommand::Build {
            services,
            no_cache,
            pull,
        } => {
            for image in project.build(&store, &services, &BuildSettings { no_cache, pull })? {
                println!("{}", image.reference);
            }
        }
        ComposeCommand::Up {
            services,
            build,
            no_build,
            force_recreate,
            no_recreate,
            no_deps,
            timeout,
            wait,
            wait_timeout,
            ..
        } => {
            let mut display = progress::PullDisplay::new(project.name());
            let options = UpOptions {
                build: if build {
                    BuildPolicy::Always
                } else if no_build {
                    BuildPolicy::Never
                } else {
                    BuildPolicy::Missing
                },
                recreate: if force_recreate {
                    RecreatePolicy::Always
                } else if no_recreate {
                    RecreatePolicy::Never
                } else {
                    RecreatePolicy::Changed
                },
                dependencies: !no_deps,
                timeout: Duration::from_secs(timeout),
                wait,
                wait_timeout: Duration::from_secs(wait_timeout),
            };
            for instance in project.up(&store, &services, &options, &mut |event| {
                display.update(event)
            })? {
                println!("{}\t{}", instance.name, instance.state);
            }
        }
        ComposeCommand::Start(args) => {
            for instance in project.start(&store, &args.services)? {
                println!("{}\t{}", instance.name, instance.state);
            }
        }
        ComposeCommand::Stop { timeout, services } => {
            project.stop(&store, &services, Duration::from_secs(timeout))?;
        }
        ComposeCommand::Down {
            timeout,
            remove_orphans,
            volumes,
        } => {
            project.down(
                &store,
                Duration::from_secs(timeout),
                remove_orphans,
                volumes,
            )?;
        }
        ComposeCommand::Ps {
            all,
            quiet,
            services,
        } => {
            if !quiet {
                println!("NAME\tSERVICE\tSTATUS\tCONTAINER ID");
            }
            for instance in project.instances(&store, &services)? {
                if !all && instance.state != "running" {
                    continue;
                }
                if quiet {
                    println!("{}", instance.id);
                } else {
                    let health = store
                        .health(&instance.id)?
                        .map(|status| format!(" ({})", status.state))
                        .unwrap_or_default();
                    println!(
                        "{}\t{}\t{}{health}\t{}",
                        instance.name,
                        instance.compose.as_ref().unwrap().service,
                        instance.state,
                        instance.id
                    );
                }
            }
        }
        ComposeCommand::Logs { follow, services } => {
            let instances = project.instances(&store, &services)?;
            let cancelled = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|scope| -> anyhow::Result<()> {
                let mut jobs = vec![];
                for instance in instances {
                    let store = &store;
                    let cancelled = &cancelled;
                    jobs.push(scope.spawn(move || {
                        let service = &instance.compose.as_ref().unwrap().service;
                        let result = store.logs_until(
                            &instance.id,
                            follow,
                            &mut Prefix::new(service, io::stdout()),
                            &mut Prefix::new(service, io::stderr()),
                            &|| cancelled.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        if result.is_err() {
                            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        result
                    }));
                }
                for job in jobs {
                    job.join()
                        .map_err(|_| anyhow::anyhow!("Compose log reader panicked"))??;
                }
                Ok(())
            })?;
        }
        ComposeCommand::Exec {
            no_tty,
            interactive,
            env,
            cwd,
            user,
            mut invocation,
        } => {
            let instances = project.instances(&store, &invocation[..1])?;
            ensure!(
                instances.len() == 1,
                "service has no unique container; use compose up -d"
            );
            invocation[0] = instances[0].id.clone();
            return super::run(DockerCommand::Exec(Exec {
                session: false,
                interactive,
                tty: !no_tty,
                env,
                env_files: vec![],
                cwd,
                user,
                invocation,
            }));
        }
    }
    Ok(0)
}

struct Prefix<W> {
    prefix: Vec<u8>,
    line_start: bool,
    writer: W,
}
impl<W> Prefix<W> {
    fn new(service: &str, writer: W) -> Self {
        Self {
            prefix: format!("{service} | ").into_bytes(),
            line_start: true,
            writer,
        }
    }
}
impl<W: Write> Write for Prefix<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.line_start {
                output.extend_from_slice(&self.prefix);
            }
            output.extend_from_slice(part);
            self.line_start = part.ends_with(b"\n");
        }
        self.writer.write_all(&output)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_log_prefix_preserves_binary_and_fragmented_lines() {
        let mut output = Prefix::new("web", Vec::new());
        output.write_all(b"fir").unwrap();
        output.write_all(b"st\nsecond\n\xff").unwrap();
        output.write_all(b"\n").unwrap();
        assert_eq!(output.writer, b"web | first\nweb | second\nweb | \xff\n");
    }
}

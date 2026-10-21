//! Docker command syntax; storage, image preparation and VM ownership live in pvisor.
use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};
use pvisor::docker::{
    self, GuestReply, GuestRequest, GuestStdin, Store, read_guest_frame, write_guest_frame,
};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Args)]
#[command(
    about = "Docker command subset using one rootless VM per container",
    long_about = "Docker command subset using one rootless VM per container.\nNo Docker daemon or global Linux VM is required.\nOnly options listed in each command's help are supported; unknown options fail.\nDocker Engine API, build, Compose, UDP, IPv6 and terminal resizing are not implemented."
)]
pub(super) struct DockerArgs {
    #[command(subcommand)]
    command: DockerCommand,
}

impl DockerArgs {
    pub(super) fn failure_exit_code(&self) -> i32 {
        if matches!(self.command, DockerCommand::Run(_)) {
            125
        } else {
            1
        }
    }
}

#[derive(Debug, Subcommand)]
enum DockerCommand {
    /// Create and run a new container in its own VM.
    Run(Box<Run>),
    /// Download an image from an OCI registry.
    Pull { image: String },
    /// List locally pulled images.
    Images(ListImages),
    /// Remove one or more local image references.
    Rmi(RemoveImages),
    /// Execute a command inside a running container VM.
    Exec(Exec),
    /// List containers.
    Ps(List),
    /// Start one or more stopped containers, retaining their writable files.
    Start(Select),
    /// Stop one or more running containers.
    Stop(Stop),
    /// Remove one or more containers.
    Rm(Remove),
    /// Fetch container stdout and stderr.
    Logs(Logs),
}

#[derive(Debug, Args)]
struct Run {
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
    #[arg(short = 'v', long = "volume")]
    volumes: Vec<String>,
    #[arg(short = 'p', long = "publish")]
    ports: Vec<String>,
    /// Native vCPU count; fractional Docker CPU quotas are unsupported.
    #[arg(long, default_value_t = 2)]
    cpus: u8,
    #[arg(short = 'm', long, default_value = "512m", value_parser = memory)]
    memory: u32,
    /// Public IPv4 TCP/DNS egress, or no network device.
    #[arg(long, default_value = "bridge", value_parser = ["bridge", "none"])]
    network: String,
    /// Image followed by the optional command and its literal arguments.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, value_names = ["IMAGE", "COMMAND"])]
    invocation: Vec<String>,
}

#[derive(Debug, Args)]
struct Select {
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
struct Stop {
    #[arg(short = 't', long, default_value_t = 10)]
    timeout: u64,
    #[arg(short = 's', long)]
    signal: Option<String>,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
struct Remove {
    #[arg(short, long)]
    force: bool,
    #[arg(short, long)]
    volumes: bool,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
struct RemoveImages {
    #[arg(short, long)]
    force: bool,
    #[arg(required = true, num_args = 1..)]
    images: Vec<String>,
}
#[derive(Debug, Args)]
struct List {
    #[arg(short, long)]
    all: bool,
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
}
#[derive(Debug, Args)]
struct ListImages {
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
    repository: Option<String>,
}
#[derive(Debug, Args)]
struct Logs {
    #[arg(short, long)]
    follow: bool,
    container: String,
}
#[derive(Debug, Args)]
struct Exec {
    #[arg(short, long)]
    interactive: bool,
    #[arg(short = 't', long)]
    tty: bool,
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    #[arg(short = 'w', long = "workdir")]
    cwd: Option<PathBuf>,
    #[arg(short = 'u', long)]
    user: Option<String>,
    /// Container followed by the executable and its literal arguments.
    #[arg(required = true, num_args = 2.., trailing_var_arg = true, value_names = ["CONTAINER", "COMMAND"])]
    invocation: Vec<String>,
}

fn memory(value: &str) -> Result<u32, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split].parse().map_err(|_| "invalid memory size")?;
    let scale = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return Err("unsupported memory suffix".into()),
    };
    let bytes = number.checked_mul(scale).ok_or("memory size overflow")?;
    let mib = bytes.div_ceil(1024 * 1024);
    if mib < 16 {
        return Err("guest memory must be at least 16 MiB".into());
    }
    u32::try_from(mib).map_err(|_| "memory size overflow".into())
}

fn environment(
    values: &[String],
    files: &[PathBuf],
) -> anyhow::Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut env = BTreeMap::new();
    let mut unset = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    for file in files {
        for line in std::fs::read_to_string(file)?.lines() {
            let line = line.trim_start();
            if !line.is_empty() && !line.starts_with('#') {
                entries.push(line.to_owned());
            }
        }
    }
    entries.extend_from_slice(values);
    for entry in entries {
        let (key, value) = match entry.split_once('=') {
            Some((key, value)) => (key, Some(value.to_owned())),
            None => (entry.as_str(), std::env::var(&entry).ok()),
        };
        ensure!(
            !key.is_empty() && !key.contains('\0'),
            "invalid environment name"
        );
        if let Some(value) = value {
            ensure!(!value.contains('\0'), "environment contains NUL");
            env.insert(key.into(), value);
            unset.remove(key);
        } else {
            env.remove(key);
            unset.insert(key.to_owned());
        }
    }
    Ok((env, unset.into_iter().collect()))
}

fn volume(value: &str, store: &Store) -> anyhow::Result<docker::Mount> {
    let parts: Vec<_> = value.split(':').collect();
    ensure!(
        (1..=3).contains(&parts.len()),
        "invalid volume specification"
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
        matches!(mode, "ro" | "rw"),
        "unsupported volume mode: {mode}"
    );
    let (source, managed) = match source {
        Some(source) if source.starts_with('/') || source.starts_with('.') => {
            let path = PathBuf::from(source)
                .canonicalize()
                .context("bind source must be an existing directory")?;
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
        volume: managed,
    })
}

fn port(value: &str) -> anyhow::Result<docker::Port> {
    let value = value.strip_suffix("/tcp").unwrap_or(value);
    ensure!(
        !value.contains('/'),
        "only TCP port publishing is supported"
    );
    let parts: Vec<_> = value.split(':').collect();
    let (address, host, guest) = match parts.as_slice() {
        [guest] => ("0.0.0.0", "0", *guest),
        [host, guest] => ("0.0.0.0", if host.is_empty() { "0" } else { host }, *guest),
        [address, host, guest] => (*address, if host.is_empty() { "0" } else { host }, *guest),
        _ => bail!("invalid port mapping"),
    };
    let guest = guest.parse()?;
    ensure!(guest != 0, "guest port must be nonzero");
    Ok(docker::Port {
        address: address.parse()?,
        host: host.parse()?,
        guest,
    })
}

pub(super) fn run(args: DockerArgs) -> anyhow::Result<i32> {
    let store = Store::discover()?;
    match args.command {
        DockerCommand::Pull { image } => {
            let image = store.pull(&image)?;
            println!("Digest: {}\n{}", image.digest, image.reference);
        }
        DockerCommand::Images(args) => {
            if !args.quiet {
                println!("REPOSITORY\tTAG\tIMAGE ID");
            }
            let mut ids = std::collections::BTreeSet::new();
            for image in store.images()? {
                let (repository, tag) = split_tag(&image.reference);
                if args
                    .repository
                    .as_ref()
                    .is_some_and(|filter| filter != &image.reference && filter != repository)
                {
                    continue;
                }
                let id = if args.no_trunc {
                    image.id.as_str()
                } else {
                    &image.id[7..19]
                };
                if args.quiet {
                    if ids.insert(id.to_owned()) {
                        println!("{id}");
                    }
                } else {
                    println!("{repository}\t{tag}\t{id}");
                }
            }
        }
        DockerCommand::Rmi(args) => {
            return batch(&args.images, |selector| {
                for tag in store.rmi(selector, args.force)? {
                    println!("Untagged: {tag}");
                }
                Ok(())
            });
        }
        DockerCommand::Ps(args) => {
            if !args.quiet {
                println!("CONTAINER ID\tIMAGE\tCOMMAND\tSTATUS\tPORTS\tNAMES");
            }
            for instance in store.instances(args.all)? {
                let id = if args.no_trunc {
                    &instance.id
                } else {
                    &instance.id[..12]
                };
                if args.quiet {
                    println!("{id}");
                } else {
                    let status = if instance.state == "exited" {
                        format!("Exited ({})", instance.exit_code.unwrap_or(125))
                    } else if instance.state == "running" {
                        "Up".into()
                    } else {
                        instance.state.clone()
                    };
                    let ports = instance
                        .config
                        .ports
                        .iter()
                        .map(|p| format!("{}:{}->{}/tcp", p.address, p.host, p.guest))
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!(
                        "{id}\t{}\t{:?}\t{status}\t{ports}\t{}",
                        instance.image.reference,
                        instance.argv.join(" "),
                        instance.name
                    );
                }
            }
        }
        DockerCommand::Run(args) => {
            ensure!(
                !(args.tty && args.interactive && !args.detach) || unsafe { libc::isatty(0) } == 1,
                "the input device is not a TTY"
            );
            let (env, unset_env) = environment(&args.env, &args.env_files)?;
            let config = docker::Create {
                name: args.name,
                image: args.invocation[0].clone(),
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
                auto_remove: args.rm,
                interactive: args.interactive,
                tty: args.tty.then(terminal_size),
                network: args.network != "none",
            };
            let instance = store.create(config)?;
            let output = store.output(&instance)?;
            let started = store.start(&instance.id)?;
            let _terminal = Terminal::new(args.tty && !args.detach, args.interactive)?;
            let _signals = if args.detach {
                None
            } else {
                Some(Signals::forward(store.clone(), started.clone())?)
            };
            if args.interactive && !args.detach && started.state != "exited" {
                let mut input = store.connect(&started)?;
                write_guest_frame(&mut input, &GuestRequest::Input {})?;
                ensure!(
                    matches!(
                        read_guest_frame::<GuestReply>(&mut input)?,
                        GuestReply::Ready
                    ),
                    "guest stdin unavailable"
                );
                input.set_read_timeout(None)?;
                input.set_write_timeout(None)?;
                std::thread::spawn(move || {
                    let _ = send_stdin(&mut input);
                    let _ = read_guest_frame::<GuestReply>(&mut input);
                });
            }
            if args.detach {
                println!("{}", instance.id);
            } else {
                return output.follow(&started.generation, &mut io::stdout(), &mut io::stderr());
            }
        }
        DockerCommand::Start(args) => {
            return batch(&args.containers, |selector| {
                store.start(selector)?;
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Stop(args) => {
            return batch(&args.containers, |selector| {
                store.stop(
                    selector,
                    Duration::from_secs(args.timeout),
                    args.signal.as_deref(),
                )?;
                println!("{selector}");
                Ok(())
            });
        }
        DockerCommand::Rm(args) => {
            return batch(&args.containers, |selector| {
                if args.force {
                    store.stop(selector, Duration::ZERO, Some("SIGKILL"))?;
                }
                store.remove(selector, args.volumes)?;
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
            use pvisor_container::api::{ExecRequest, ExecRuntime, ExecSession, ExecStartError};
            let stream = store.connect(&instance)?;
            let (env, unset_env) = environment(&args.env, &[])?;
            let request = ExecRequest {
                argv: args.invocation[1..].to_vec(),
                env,
                unset_env,
                cwd: args.cwd,
                user: args.user,
                tty: args.tty.then(terminal_size),
                ..Default::default()
            };
            let session = match ExecSession::start(stream, request) {
                Ok(session) => session,
                Err(ExecStartError::Rejected { message, code }) => {
                    eprintln!("{message}");
                    return Ok(code);
                }
                Err(error) => return Err(error.into()),
            };
            let input = if args.interactive {
                use std::os::fd::AsFd;
                Some(std::fs::File::from(
                    io::stdin().as_fd().try_clone_to_owned()?,
                ))
            } else {
                None
            };
            let outcome = session.relay(input, &mut io::stdout(), &mut io::stderr())?;
            ensure!(outcome.output_complete, "guest exec output was incomplete");
            return Ok(outcome.code);
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
                    write_guest_frame(&mut stream, &GuestRequest::Stop { signal })?;
                    ensure!(
                        matches!(
                            read_guest_frame::<GuestReply>(&mut stream)?,
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

fn send_stdin(stream: &mut std::os::unix::net::UnixStream) -> anyhow::Result<()> {
    let mut buffer = [0; 16384];
    loop {
        let size = io::stdin().read(&mut buffer)?;
        if size == 0 {
            write_guest_frame(stream, &GuestStdin::Eof {})?;
            return Ok(());
        }
        write_guest_frame(
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

fn batch(
    values: &[String],
    mut action: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<i32> {
    let mut code = 0;
    for value in values {
        if let Err(error) = action(value) {
            eprintln!("Error: {error:#}");
            code = 1;
        }
    }
    Ok(code)
}

fn split_tag(reference: &str) -> (&str, &str) {
    match reference.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository, tag),
        _ => (reference, "latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        args: DockerArgs,
    }
    #[test]
    fn command_names_and_guest_arguments_have_separate_boundaries() {
        let parsed = Cli::try_parse_from([
            "docker",
            "run",
            "--name",
            "test",
            "alpine",
            "sh",
            "-c",
            "echo --privileged",
        ])
        .unwrap();
        let DockerCommand::Run(args) = parsed.args.command else {
            panic!()
        };
        assert_eq!(args.invocation, ["alpine", "sh", "-c", "echo --privileged"]);
        assert!(Cli::try_parse_from(["docker", "run", "--privileged", "alpine"]).is_err());
        let parsed = Cli::try_parse_from(["docker", "run", "alpine", "--help"]).unwrap();
        let DockerCommand::Run(args) = parsed.args.command else {
            panic!()
        };
        assert_eq!(args.invocation, ["alpine", "--help"]);
        assert!(Cli::try_parse_from(["docker", "build", "."]).is_err());
        for command in [
            "run", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm", "logs",
        ] {
            let error = Cli::try_parse_from(["docker", command, "--help"]).unwrap_err();
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

//! Docker command syntax; storage, image preparation and VM ownership live in pvisor.
use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};
use pvisor::docker::{
    self, GuestReply, GuestRequest, GuestStdin, Store, read_guest_frame, write_guest_frame,
};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

mod build;
mod network;
mod progress;

impl DockerCommand {
    pub(super) fn failure_exit_code(&self) -> i32 {
        if matches!(self, DockerCommand::Run(_)) {
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
    /// Create and run a new container in its own VM.
    Run(Box<Run>),
    /// Manage container networks.
    Network(network::NetworkArgs),
    /// Download an image from an OCI registry.
    Pull {
        /// Select an image platform; downloading does not enable CPU emulation.
        #[arg(long, value_parser = ["linux/amd64", "linux/arm64", "linux/arm64/v8"])]
        platform: Option<String>,
        image: String,
    },
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
pub(super) struct Run {
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
    #[arg(short = 'v', long = "volume")]
    volumes: Vec<String>,
    #[arg(short = 'p', long = "publish")]
    ports: Vec<String>,
    /// Native vCPU count; fractional Docker CPU quotas are unsupported.
    #[arg(long, default_value_t = 2)]
    cpus: u8,
    #[arg(short = 'm', long, default_value = "512m", value_parser = memory)]
    memory: u32,
    /// Connect to bridge (default), none, or a user-defined network.
    #[arg(long, alias = "net", default_value = "bridge")]
    network: String,
    /// Static IPv4 in a user-defined network's configured subnet.
    #[arg(long)]
    ip: Option<std::net::Ipv4Addr>,
    /// Image followed by the optional command and its literal arguments.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, value_names = ["IMAGE", "COMMAND"])]
    invocation: Vec<String>,
}

#[derive(Debug, Args)]
pub(super) struct Select {
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct Stop {
    #[arg(short = 't', long, default_value_t = 10)]
    timeout: u64,
    #[arg(short = 's', long)]
    signal: Option<String>,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(Debug, Args)]
pub(super) struct Remove {
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
}
#[derive(Debug, Args)]
pub(super) struct ListImages {
    #[arg(short, long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
    repository: Option<String>,
}
#[derive(Debug, Args)]
pub(super) struct Logs {
    #[arg(short, long)]
    follow: bool,
    container: String,
}
#[derive(Debug, Args)]
pub(super) struct Exec {
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

pub(super) fn run(command: DockerCommand) -> anyhow::Result<i32> {
    if let DockerCommand::Build(args) = command {
        return build::run(args);
    }
    if let DockerCommand::Network(args) = command {
        return network::run(args);
    }
    let store = Store::discover()?;
    match command {
        DockerCommand::Build(_) | DockerCommand::Network(_) => unreachable!(),
        DockerCommand::Pull { image, platform } => {
            let image = {
                let mut display = progress::PullDisplay::new(&image);
                let mut progress = |event| display.update(event);
                match platform {
                    Some(platform) => {
                        store.pull_for_platform_with_progress(&image, &platform, &mut progress)?
                    }
                    None => store.pull_with_progress(&image, &mut progress)?,
                }
            };
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
            ensure!(
                !args.network.starts_with("container:") && args.network != "host",
                "network mode is unsupported: {}",
                args.network
            );
            let selected_network = store.inspect_network(&args.network)?;
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
                auto_remove: args.rm,
                interactive: args.interactive,
                tty: args.tty.then(terminal_size),
                network: selected_network.name != "none",
                network_name: Some(selected_network.name),
                ipv4_address: args.ip,
                peers: None,
            };
            let instance = {
                let mut display = progress::PullDisplay::new(&config.image);
                store.create_with_progress(config, &mut |event| display.update(event))?
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
            let mut stream = store.connect(&instance)?;
            let (mut env, unset_env) = environment(&args.env, &[])?;
            if args.tty
                && !instance.env.contains_key("TERM")
                && !unset_env.iter().any(|key| key == "TERM")
            {
                env.entry("TERM".into()).or_insert_with(|| "xterm".into());
            }
            write_guest_frame(
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
            let first: GuestReply = read_guest_frame(&mut stream)?;
            match first {
                GuestReply::Ready => {}
                GuestReply::Error { message } => {
                    eprintln!("{message}");
                    if let Ok(GuestReply::Exit { code }) = read_guest_frame(&mut stream) {
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
                write_guest_frame(&mut stream, &GuestStdin::Eof {})?;
            }
            loop {
                match read_guest_frame(&mut stream)? {
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
        #[command(subcommand)]
        command: DockerCommand,
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
        assert_eq!(args.network, "dev");
        assert_eq!(args.ip, Some("172.18.0.11".parse().unwrap()));
        let parsed = Cli::try_parse_from(["pvisor", "run", "--net=dev", "alpine"]).unwrap();
        let DockerCommand::Run(args) = parsed.command else {
            panic!()
        };
        assert_eq!(args.network, "dev");
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
    fn docker_platform_options_precede_the_image_and_preserve_guest_flags() {
        let parsed =
            Cli::try_parse_from(["pvisor", "pull", "--platform=linux/amd64", "alpine"]).unwrap();
        let DockerCommand::Pull { platform, image } = parsed.command else {
            panic!()
        };
        assert_eq!(platform.as_deref(), Some("linux/amd64"));
        assert_eq!(image, "alpine");
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

//! Windows frontend for native container VMs and direct execution.
use crate::container_args::{PsRow, batch, environment, memory, ps_header, ps_row};
use crate::windows_terminal::{StdinSource, Terminal};
use clap::{Parser, Subcommand};
use pvisor_containers::api::{Containers, Store};
use std::{io, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    name = "pvisor",
    version,
    about = "pVisor Windows containers (WHP; bridge networking and interactive guest terminals)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pull a public linux/amd64 image; downloads run on the Windows host.
    Pull { image: String },
    /// List locally verified images.
    Images(Images),
    /// Remove image catalog references, retaining blobs needed by containers.
    Rmi(RemoveImages),
    /// Run a container in its own WHP VM; bridge networking is the default.
    #[command(disable_help_flag = true)]
    Run(Run),
    /// List running containers; -a also includes stopped/unknown instances.
    Ps(List),
    /// Execute another command in a running container; -it attaches a guest PTY.
    Exec(Exec),
    /// Read workload stdout/stderr; -f follows the selected generation to exit.
    Logs(Logs),
    /// Start stopped containers, preserving their private writable filesystem.
    Start(Select),
    /// Send the image stop signal (or -s); -t -1 waits without force-stop.
    Stop(Stop),
    /// Remove retained containers; -f first stops a live one.
    Rm(Remove),
    /// Docker's grouped container spellings.
    Container {
        #[command(subcommand)]
        command: ContainerCommand,
    },
    /// Docker's grouped image spellings.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Execute a Linux command in a caller-owned rootfs; writes persist directly.
    VmRun(crate::vm_run::Args),
}

#[derive(Subcommand)]
enum ContainerCommand {
    #[command(disable_help_flag = true)]
    Run(Run),
    Exec(Exec),
    #[command(alias = "list", alias = "ps")]
    Ls(List),
    Start(Select),
    Stop(Stop),
    #[command(alias = "remove")]
    Rm(Remove),
    Logs(Logs),
}
#[derive(Subcommand)]
enum ImageCommand {
    Pull {
        image: String,
    },
    #[command(alias = "list")]
    Ls(Images),
    #[command(alias = "remove")]
    Rm(RemoveImages),
}
impl Command {
    fn ungroup(self) -> Self {
        match self {
            Self::Container { command } => match command {
                ContainerCommand::Run(args) => Self::Run(args),
                ContainerCommand::Exec(args) => Self::Exec(args),
                ContainerCommand::Ls(args) => Self::Ps(args),
                ContainerCommand::Start(args) => Self::Start(args),
                ContainerCommand::Stop(args) => Self::Stop(args),
                ContainerCommand::Rm(args) => Self::Rm(args),
                ContainerCommand::Logs(args) => Self::Logs(args),
            },
            Self::Image { command } => match command {
                ImageCommand::Pull { image } => Self::Pull { image },
                ImageCommand::Ls(args) => Self::Images(args),
                ImageCommand::Rm(args) => Self::Rmi(args),
            },
            command => command,
        }
    }
}
#[derive(clap::Args)]
struct Images {
    #[arg(long)]
    json: bool,
}
#[derive(clap::Args)]
struct RemoveImages {
    #[arg(short = 'f', long)]
    force: bool,
    #[arg(required = true, num_args = 1..)]
    images: Vec<String>,
}
#[derive(clap::Args)]
struct List {
    #[arg(short = 'a', long)]
    all: bool,
    #[arg(short = 'q', long)]
    quiet: bool,
    #[arg(long)]
    no_trunc: bool,
    #[arg(long)]
    json: bool,
}
#[derive(clap::Args)]
struct Select {
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(clap::Args)]
struct Stop {
    #[arg(short = 't', long, default_value_t = 10, allow_hyphen_values = true, value_parser = clap::value_parser!(i64).range(-1..))]
    timeout: i64,
    #[arg(short = 's', long)]
    signal: Option<String>,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(clap::Args)]
struct Remove {
    #[arg(short = 'f', long)]
    force: bool,
    #[arg(required = true, num_args = 1..)]
    containers: Vec<String>,
}
#[derive(clap::Args)]
struct Logs {
    #[arg(short = 'f', long)]
    follow: bool,
    container: String,
}
#[derive(clap::Args)]
struct Exec {
    #[arg(short = 'i', long)]
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
    container: String,
    #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(clap::Args)]
struct Run {
    #[arg(short = 'i', long)]
    interactive: bool,
    #[arg(short = 't', long)]
    tty: bool,
    #[arg(long = "help", action = clap::ArgAction::Help)]
    help: Option<bool>,
    #[arg(long)]
    name: Option<String>,
    #[arg(short = 'd', long)]
    detach: bool,
    #[arg(long)]
    rm: bool,
    /// KEY=VALUE or KEY to explicitly forward that one host variable.
    #[arg(short='e',long="env",action=clap::ArgAction::Append)]
    env: Vec<String>,
    #[arg(long = "env-file")]
    env_files: Vec<PathBuf>,
    #[arg(short = 'w', long = "workdir")]
    cwd: Option<PathBuf>,
    #[arg(short = 'u', long)]
    user: Option<String>,
    #[arg(long)]
    entrypoint: Option<String>,
    /// Whole vCPUs, not fractional CPU quotas.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=64))]
    cpus: u8,
    /// Guest RAM; Docker byte/k/m/g syntax, minimum 128 MiB on Windows.
    #[arg(short = 'm', long, default_value = "512m", value_parser = memory)]
    memory: u32,
    /// Default userspace bridge, or an explicitly offline guest.
    #[arg(long, alias = "net", default_value = "bridge", value_parser = ["bridge", "none"])]
    network: String,
    #[arg(required=true,num_args=1..,trailing_var_arg=true,value_names=["IMAGE","COMMAND"])]
    invocation: Vec<String>,
}

/// Parse Windows commands after dispatching private owner/runner modes. Session,
/// mount, port, build and Engine/Compose profiles are rejected.
pub fn main() -> anyhow::Result<()> {
    match Store::internal_if_requested() {
        Ok(true) => return Ok(()),
        Ok(false) => (),
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::exit(125);
        }
    }
    let command = Cli::parse().command.ungroup();
    let failure = if matches!(&command, Command::Run(_)) {
        125
    } else {
        1
    };
    match dispatch(command) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::exit(failure);
        }
    }
}

fn dispatch(command: Command) -> anyhow::Result<i32> {
    match command {
        Command::VmRun(args) => crate::vm_run::run(args).map(|_| 0),
        command => {
            let store = Store::open(None)?;
            let (mut stdout, mut stderr) = (io::stdout().lock(), io::stderr().lock());
            match command {
                Command::Pull { image } => {
                    let image = store.pull(&image)?;
                    println!("Pulled {} {}", image.reference, image.digest);
                }
                Command::Images(Images { json }) => {
                    let images = store.images()?;
                    if json {
                        println!("{}", serde_json::to_string(&images)?);
                    } else {
                        println!("REPOSITORY\tIMAGE ID\tDIGEST");
                        for image in images {
                            println!("{}\t{}\t{}", image.reference, image.id, image.digest);
                        }
                    }
                }
                Command::Run(mut args) => {
                    use std::io::IsTerminal;
                    anyhow::ensure!(
                        !(args.tty && args.interactive && !args.detach)
                            || std::io::stdin().is_terminal(),
                        "the input device is not a TTY"
                    );
                    let (env, unset_env) = environment(&args.env, &args.env_files)?;
                    let image = args.invocation.remove(0);
                    let container = store.run(pvisor_containers::api::Run {
                        network: if args.network == "bridge" {
                            pvisor_containers::api::Network::Bridge
                        } else {
                            pvisor_containers::api::Network::None
                        },
                        image,
                        name: args.name,
                        command: args.invocation,
                        env,
                        unset_env,
                        cwd: args.cwd,
                        user: args.user,
                        entrypoint: args.entrypoint,
                        cpus: args.cpus,
                        memory_mib: args.memory,
                        detach: args.detach,
                        auto_remove: args.rm,
                        interactive: args.interactive,
                        tty: args.tty.then(Terminal::size),
                    })?;
                    if args.detach {
                        println!("{}", container.id);
                    } else {
                        let attachment = (|| {
                            let terminal = Terminal::new(args.tty, args.interactive)?;
                            let input = if args.interactive {
                                Some(StdinSource::new()?)
                            } else {
                                None
                            };
                            Ok::<_, anyhow::Error>((terminal, input))
                        })();
                        let (_terminal, mut input) = match attachment {
                            Ok(attachment) => attachment,
                            Err(error) => {
                                // No usable frontend owns this foreground run.
                                // Do not leave an unattended interactive guest.
                                store.remove(&container.id, true)?;
                                return Err(error);
                            }
                        };
                        let code = store.wait(
                            &container,
                            input
                                .as_mut()
                                .map(|i| i as &mut dyn pvisor_containers::api::InputSource),
                            &mut stdout,
                            &mut stderr,
                        )?;
                        return Ok(code);
                    }
                }
                Command::Ps(List {
                    all,
                    quiet,
                    no_trunc,
                    json,
                }) => {
                    let containers = store.list(all)?;
                    if json {
                        println!("{}", serde_json::to_string(&containers)?);
                    } else {
                        ps_header(&mut stdout, quiet)?;
                        for c in containers {
                            let state = match c.state {
                                pvisor_containers::api::State::Starting => "starting",
                                pvisor_containers::api::State::Running => "running",
                                pvisor_containers::api::State::Exited => "exited",
                                pvisor_containers::api::State::Unknown => "unknown",
                            };
                            ps_row(
                                &mut stdout,
                                PsRow {
                                    id: &c.id,
                                    image: &c.image,
                                    command: &c.command,
                                    state,
                                    exit_code: c.exit_code,
                                    ports: "",
                                    name: &c.name,
                                },
                                quiet,
                                no_trunc,
                            )?;
                        }
                    }
                }
                Command::Exec(Exec {
                    container,
                    command,
                    env,
                    env_files,
                    cwd,
                    user,
                    interactive,
                    tty,
                }) => {
                    let _terminal = Terminal::new(tty, interactive)?;
                    let mut input = if interactive {
                        Some(StdinSource::new()?)
                    } else {
                        None
                    };
                    let (env, unset_env) = environment(&env, &env_files)?;
                    let code = store.exec(
                        &container,
                        pvisor_guest::control::ExecLaunch {
                            argv: command,
                            env,
                            replace_env: false,
                            limits: Default::default(),
                            cwd,
                            user,
                            tty: tty.then(Terminal::size),
                        },
                        unset_env,
                        input
                            .as_mut()
                            .map(|i| i as &mut dyn pvisor_containers::api::InputSource),
                        &mut stdout,
                        &mut stderr,
                    )?;
                    return Ok(code);
                }
                Command::Logs(Logs { container, follow }) => {
                    store.logs(&container, follow, &mut stdout, &mut stderr)?
                }
                Command::Start(Select { containers }) => {
                    return batch(&containers, |container| {
                        store.start(container)?;
                        println!("{container}");
                        Ok(())
                    });
                }
                Command::Rmi(RemoveImages { force, images }) => {
                    return batch(&images, |image| {
                        for removed in store.remove_image(image, force)? {
                            println!("Untagged: {}", removed.reference);
                        }
                        Ok(())
                    });
                }
                Command::Stop(Stop {
                    timeout,
                    containers,
                    signal,
                }) => {
                    return batch(&containers, |container| {
                        store.stop(
                            container,
                            if timeout == -1 {
                                Duration::MAX
                            } else {
                                Duration::from_secs(timeout as u64)
                            },
                            signal.as_deref(),
                        )?;
                        println!("{container}");
                        Ok(())
                    });
                }
                Command::Rm(Remove { force, containers }) => {
                    return batch(&containers, |container| {
                        store.remove(container, force)?;
                        println!("{container}");
                        Ok(())
                    });
                }
                Command::Container { .. } | Command::Image { .. } => unreachable!(),
                Command::VmRun(_) => unreachable!(),
            }
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn grouped_aliases_stop_and_log_options_match_the_unix_spellings() {
        for prefix in [vec!["pvisor", "run"], vec!["pvisor", "container", "run"]] {
            let mut words = prefix.clone();
            words.extend(["--net", "none", "--rm", "alpine", "echo", "--guest-flag"]);
            let Command::Run(args) = Cli::try_parse_from(words).unwrap().command.ungroup() else {
                panic!("run alias did not normalize");
            };
            assert_eq!(args.network, "none");
            assert_eq!(args.invocation, ["alpine", "echo", "--guest-flag"]);
            let mut help = prefix.clone();
            help.push("--help");
            assert_eq!(
                Cli::try_parse_from(help).err().unwrap().kind(),
                ErrorKind::DisplayHelp
            );
            let mut reserved = prefix;
            reserved.push("-h");
            assert_ne!(
                Cli::try_parse_from(reserved).err().unwrap().kind(),
                ErrorKind::DisplayHelp
            );
        }
        for spelling in ["ls", "list", "ps"] {
            let Command::Ps(args) =
                Cli::try_parse_from(["pvisor", "container", spelling, "-aq", "--no-trunc"])
                    .unwrap()
                    .command
                    .ungroup()
            else {
                panic!("list alias did not normalize");
            };
            assert!(args.all && args.quiet && args.no_trunc);
        }
        for spelling in ["rm", "remove"] {
            assert!(matches!(
                Cli::try_parse_from(["pvisor", "container", spelling, "-f", "one", "two",])
                    .unwrap()
                    .command
                    .ungroup(),
                Command::Rm(_)
            ));
            assert!(matches!(
                Cli::try_parse_from(["pvisor", "image", spelling, "-f", "alpine",])
                    .unwrap()
                    .command
                    .ungroup(),
                Command::Rmi(_)
            ));
        }
        for spelling in ["ls", "list"] {
            assert!(matches!(
                Cli::try_parse_from(["pvisor", "image", spelling,])
                    .unwrap()
                    .command
                    .ungroup(),
                Command::Images(_)
            ));
        }
        for prefix in [vec!["pvisor", "stop"], vec!["pvisor", "container", "stop"]] {
            let mut words = prefix;
            words.extend(["-t", "-1", "-s", "SIGUSR1", "one", "two"]);
            let Command::Stop(args) = Cli::try_parse_from(words).unwrap().command.ungroup() else {
                panic!("stop alias did not normalize");
            };
            assert_eq!(args.timeout, -1);
            assert_eq!(args.signal.as_deref(), Some("SIGUSR1"));
            assert_eq!(args.containers, ["one", "two"]);
        }
        assert!(Cli::try_parse_from(["pvisor", "stop", "-t", "-2", "box"]).is_err());
        let Command::Stop(defaults) = Cli::try_parse_from(["pvisor", "stop", "box"])
            .unwrap()
            .command
        else {
            panic!("expected stop");
        };
        assert_eq!(defaults.timeout, 10);
        assert!(defaults.signal.is_none());
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "container", "logs", "-f", "box",])
                .unwrap()
                .command
                .ungroup(),
            Command::Logs(Logs { follow: true, .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "image", "pull", "alpine",])
                .unwrap()
                .command
                .ungroup(),
            Command::Pull { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "container", "exec", "box", "echo", "--literal",])
                .unwrap()
                .command
                .ungroup(),
            Command::Exec(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "container", "start", "box",])
                .unwrap()
                .command
                .ungroup(),
            Command::Start(_)
        ));
    }

    #[test]
    fn native_profile_rejects_unported_workflows_and_capabilities() {
        assert!(
            Cli::try_parse_from(["pvisor", "vm-run", "--rootfs", "root", "--", "/probe"]).is_ok()
        );
        for command in ["daemon", "tui", "replay", "spawn", "build", "network"] {
            let error = Cli::try_parse_from(["pvisor", command]).err().unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{command}");
        }
        for flag in ["-v", "-p", "--privileged"] {
            assert!(
                Cli::try_parse_from(["pvisor", "run", flag, "alpine"]).is_err(),
                "{flag}"
            );
        }
        assert!(Cli::try_parse_from(["pvisor", "run", "--network", "host", "alpine"]).is_err());
        assert!(
            Cli::try_parse_from(["pvisor", "run", "-it", "--network", "bridge", "alpine"]).is_ok()
        );
        for args in [
            vec![
                "pvisor",
                "run",
                "--rm",
                "-e",
                "GREETING=hello world",
                "alpine",
                "echo",
                "hello",
            ],
            vec![
                "pvisor", "run", "-d", "--name", "box", "alpine", "sleep", "300",
            ],
            vec!["pvisor", "pull", "alpine"],
            vec!["pvisor", "images"],
            vec!["pvisor", "rmi", "-f", "alpine"],
            vec!["pvisor", "start", "box"],
            vec!["pvisor", "ps", "-a"],
            vec!["pvisor", "exec", "box", "sh", "-c", "echo ok"],
            vec!["pvisor", "logs", "box"],
            vec!["pvisor", "stop", "box"],
            vec!["pvisor", "rm", "box"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn windows_resource_and_exec_options_preserve_literal_values() {
        let Cli {
            command: Command::Run(run),
        } = Cli::try_parse_from([
            "pvisor",
            "run",
            "--cpus",
            "3",
            "-m",
            "1g",
            "-w",
            "/work",
            "-u",
            "1000:1000",
            "--entrypoint",
            "/bin/sh",
            "alpine",
            "-c",
            "echo literal",
        ])
        .unwrap()
        else {
            panic!("expected run");
        };
        assert_eq!((run.cpus, run.memory), (3, 1024));
        assert_eq!(run.cwd.unwrap(), PathBuf::from("/work"));
        assert_eq!(run.invocation, ["alpine", "-c", "echo literal"]);
        assert!(Cli::try_parse_from(["pvisor", "run", "--cpus", "0", "alpine"]).is_err());
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "exec",
                "-e",
                "A=literal $HOME",
                "-w",
                "/",
                "-u",
                "1000",
                "box",
                "echo",
                "hello world",
            ])
            .is_ok()
        );
    }
}

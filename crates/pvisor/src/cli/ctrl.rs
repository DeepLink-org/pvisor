//! Explicit, attempt-scoped host controls, separate from Job checkpoint commands.
use crate::runtime::instance_control::{
    INSTANCE_CONTROL_VERSION, InstanceControlCommand, InstanceControlRequest,
    InstanceControlResponse, exchange,
};
use clap::{Args, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(super) struct CtrlArgs {
    #[arg(long, value_name = "PATH")]
    socket: PathBuf,
    #[arg(long, value_name = "ID", value_parser = nonempty_identity)]
    run_id: String,
    #[arg(long, value_name = "ID", value_parser = nonempty_identity)]
    attempt_id: String,
    #[command(subcommand)]
    command: CtrlCommand,
}

fn nonempty_identity(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("identity must not be empty".into())
    } else {
        Ok(value.to_owned())
    }
}

#[derive(Debug, Subcommand)]
enum CtrlCommand {
    Pause,
    Resume,
    Offload {
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Reload an offloaded live attempt (not snapshot restoration).
    Load,
    Status,
}

impl CtrlArgs {
    fn request(self) -> (PathBuf, InstanceControlRequest) {
        let (command, file) = match self.command {
            CtrlCommand::Pause => (InstanceControlCommand::Pause, None),
            CtrlCommand::Resume => (InstanceControlCommand::Resume, None),
            CtrlCommand::Offload { file } => (InstanceControlCommand::Offload, file),
            CtrlCommand::Load => (InstanceControlCommand::Load, None),
            CtrlCommand::Status => (InstanceControlCommand::Status, None),
        };
        (
            self.socket,
            InstanceControlRequest {
                version: INSTANCE_CONTROL_VERSION,
                run_id: self.run_id.into(),
                attempt_id: self.attempt_id.into(),
                command,
                file,
            },
        )
    }
}

fn emit(response: &InstanceControlResponse, mut output: impl Write) -> anyhow::Result<()> {
    serde_json::to_writer(&mut output, response)?;
    writeln!(output)?;
    output.flush()?;
    anyhow::ensure!(
        response.ok,
        "VM control rejected: {}",
        response
            .error
            .as_deref()
            .unwrap_or("unspecified control error")
    );
    Ok(())
}

pub(super) async fn run(args: CtrlArgs) -> anyhow::Result<()> {
    let (socket, request) = args.request();
    let response = exchange(&socket, &request).await?;
    emit(&response, std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn parse(args: &[&str]) -> CtrlArgs {
        let Command::Ctrl(args) = Cli::try_parse_from(args).unwrap().command else {
            panic!("expected ctrl");
        };
        args
    }

    #[test]
    fn commands_map_to_versioned_identity_scoped_requests() {
        for (name, expected) in [
            ("pause", InstanceControlCommand::Pause),
            ("resume", InstanceControlCommand::Resume),
            ("offload", InstanceControlCommand::Offload),
            ("load", InstanceControlCommand::Load),
            ("status", InstanceControlCommand::Status),
        ] {
            let (socket, request) = parse(&[
                "pvisor",
                "ctrl",
                "--socket",
                "/private/control.sock",
                "--run-id",
                "run-one",
                "--attempt-id",
                "attempt-one",
                name,
            ])
            .request();
            assert_eq!(socket, PathBuf::from("/private/control.sock"));
            assert_eq!(request.version, 1);
            assert_eq!(request.run_id.as_str(), "run-one");
            assert_eq!(request.attempt_id.as_str(), "attempt-one");
            assert_eq!(request.command, expected);
            assert_eq!(request.file, None);
        }
    }

    #[test]
    fn identities_and_socket_are_required_even_for_status() {
        for missing in ["--socket", "--run-id", "--attempt-id"] {
            let mut args = vec!["pvisor", "ctrl"];
            for (flag, value) in [
                ("--socket", "/private/control.sock"),
                ("--run-id", "run-one"),
                ("--attempt-id", "attempt-one"),
            ] {
                if flag != missing {
                    args.extend([flag, value]);
                }
            }
            args.push("status");
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(
            Cli::try_parse_from([
                "pvisor",
                "ctrl",
                "--socket",
                "/private/control.sock",
                "--run-id",
                "",
                "--attempt-id",
                "attempt-one",
                "status",
            ])
            .is_err()
        );
    }

    #[test]
    fn file_is_optional_and_exclusive_to_offload() {
        let base = [
            "pvisor",
            "ctrl",
            "--socket",
            "/private/control.sock",
            "--run-id",
            "run-one",
            "--attempt-id",
            "attempt-one",
        ];
        let mut args = base.to_vec();
        args.extend(["offload", "--file", "/private/ram"]);
        assert_eq!(
            parse(&args).request().1.file,
            Some(PathBuf::from("/private/ram"))
        );
        for command in ["pause", "resume", "load", "status"] {
            let mut args = base.to_vec();
            args.extend([command, "--file", "/private/ram"]);
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn rejected_response_is_json_and_an_error() {
        let response: InstanceControlResponse = serde_json::from_value(serde_json::json!({
            "version": 1, "run_id": "run-one", "attempt_id": "attempt-one",
            "ok": false, "status": {
                            "run_id": "run-one", "state": "running", "updated_at_unix_ms": 0,
                            "attempt": {
                                "attempt_id": "attempt-one", "number": 1,
                                "executor": {
                                    "name": "vm", "kind": "virtual_machine", "isolation": "virtual_machine",
                                    "supports_checkpoint": false, "supports_migration": false
                                }
                            }
                        },
            "value": null, "error": "operation unavailable"
        }))
        .unwrap();
        let mut output = Vec::new();
        assert!(
            emit(&response, &mut output)
                .unwrap_err()
                .to_string()
                .contains("operation unavailable")
        );
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["ok"], false);
    }
}

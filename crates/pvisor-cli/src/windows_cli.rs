//! Windows frontend for the direct VM execution profile.
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pvisor", version, about = "pVisor Windows VM execution (WHP)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute a Linux command in a caller-owned rootfs; writes persist directly.
    VmRun(crate::vm_run::Args),
}

/// Parse Windows CLI arguments and run the selected VM. Other profiles are rejected.
pub fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::VmRun(args) => crate::vm_run::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn direct_vm_profile_does_not_advertise_unported_workflows() {
        assert!(
            Cli::try_parse_from(["pvisor", "vm-run", "--rootfs", "root", "--", "/probe"]).is_ok()
        );
        for command in ["daemon", "tui", "replay", "spawn", "run"] {
            let error = Cli::try_parse_from(["pvisor", command]).err().unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{command}");
        }
    }
}

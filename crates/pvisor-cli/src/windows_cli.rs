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

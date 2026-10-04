use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, ClapArgs)]
pub(super) struct Args {
    /// Environment store (defaults to the per-user pVisor state directory).
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, ValueEnum,
)]
#[serde(rename_all = "snake_case")]
enum RamStorage {
    #[default]
    Raw,
    Compressed,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Run an owned, snapshot-capable VM; save it from another terminal.
    Run {
        #[arg(long)]
        name: String,
        /// Import this directory into an owned base before starting the VM.
        #[arg(long, required_unless_present = "base", conflicts_with = "base")]
        rootfs: Option<PathBuf>,
        /// Reuse an already imported base generation (no full-tree import).
        #[arg(long, required_unless_present = "rootfs")]
        base: Option<String>,
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=8))]
        cpus: u8,
        #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u32).range(64..))]
        memory: u32,
        /// Persist RAM as compressed, deduplicated blocks or indexed raw bytes.
        #[arg(long, value_enum, default_value = "raw")]
        ram_storage: RamStorage,
        /// Use rootfs /init.krun directly instead of the pVisor guest launcher.
        #[arg(long, conflicts_with = "command")]
        native_init: bool,
        #[arg(last = true, required_unless_present = "native_init")]
        command: Vec<String>,
    },
    /// Import and verify a base once; prints its immutable generation identity.
    ImportBase {
        #[arg(long)]
        rootfs: PathBuf,
    },
    /// Fully audit an imported base's content and native metadata.
    VerifyBase { id: String },
    /// Freeze, publish stage plus machine/RAM state and exit the named runner.
    Save { name: String },
    /// Continue a snapshot in a new, independently owned VM instance.
    #[command(visible_alias = "fork")]
    Restore {
        id: String,
        #[arg(long)]
        name: String,
        /// Verify/materialize RAM upfront and avoid a FUSE RAM mount.
        #[arg(long)]
        eager_ram: bool,
    },
    /// List published snapshot identities.
    List,
    /// Delete a snapshot when it has no active readers.
    Delete { id: String },
    /// Reclaim abandoned writes and completed deletion tombstones.
    Gc,
    #[command(hide = true)]
    Runner { spec: PathBuf },
    #[command(hide = true)]
    RamWatchdog { mount: PathBuf },
    #[command(hide = true)]
    SocketWatchdog { directory: PathBuf },
}

pub(super) fn run(args: Args) -> anyhow::Result<()> {
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    {
        supported::run(args)
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        let _ = args;
        anyhow::bail!("complete environment snapshots require macOS on Apple Silicon")
    }
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
mod supported;

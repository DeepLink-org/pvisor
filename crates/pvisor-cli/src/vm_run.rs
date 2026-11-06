//! The common, direct VM execution profile. No Session owner or host executor is involved.
use std::path::PathBuf;

use pvisor_vm::api::{RuntimeSupport, VmBuilder, VmConfiguration, VmRuntime};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Caller-owned writable Linux rootfs. All guest writes persist here.
    #[arg(long)]
    rootfs: PathBuf,
    /// Explicit host-loadable firmware library; otherwise use the embedded kernel.
    #[arg(long)]
    firmware: Option<PathBuf>,
    /// Virtual CPU count (1–64; online CPUs also depend on firmware).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=64))]
    cpus: u8,
    /// Guest RAM in MiB (1–65536).
    #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u32).range(1..=65536))]
    memory: u32,
    /// Host directory exposed read-only at /readonly in the guest.
    #[arg(long)]
    readonly: Option<PathBuf>,
    /// Absolute Linux executable and arguments; no host shell expansion.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

pub(crate) fn run(args: Args) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.command[0].starts_with('/') && !args.command[0].contains('\0'),
        "command must be an absolute Linux executable path"
    );
    let root = args.rootfs.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "rootfs must be a directory");
    // Exposing the host root would also let guest bootstrap mount over host paths.
    anyhow::ensure!(
        root.parent().is_some(),
        "host root cannot be used as rootfs"
    );
    let mut vm = VmBuilder::new(args.cpus, args.memory)?;
    if let Some(firmware) = args.firmware {
        vm.set_firmware_path(firmware.canonicalize()?)?;
    } else {
        anyhow::ensure!(
            pvisor_vm::api::VmPlatform::embedded_kernel().is_some(),
            "no embedded kernel; supply --firmware with a host-loadable library"
        );
    }
    vm.filesystem("/dev/root", &root, 0)?;
    let mut config = pvisor_guest::GuestConfig {
        argv: args.command,
        cwd: "/".into(),
        stdio_ports: Some([false; 3]),
        ..Default::default()
    };
    if let Some(readonly) = args.readonly {
        let readonly = readonly.canonicalize()?;
        anyhow::ensure!(readonly.is_dir(), "readonly must be a directory");
        anyhow::ensure!(
            !readonly.starts_with(&root) && !root.starts_with(&readonly),
            "readonly and rootfs must be disjoint directories"
        );
        vm.read_only_filesystem("readonly", &readonly)?;
        config.shares.push(pvisor_guest::GuestShare {
            tag: "readonly".into(),
            path: "/readonly".into(),
            read_only: true,
        });
    }
    vm.virtual_file(
        "/dev/root",
        pvisor_guest::CONFIG_PATH,
        serde_json::to_vec(&config)?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    anyhow::bail!("VM returned without an authenticated guest exit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn validates_resources_and_preserves_guest_arguments() {
        let cli = Cli::try_parse_from([
            "vm-run",
            "--rootfs",
            "root",
            "--cpus",
            "2",
            "--",
            "/probe",
            "--guest-option",
        ])
        .unwrap();
        assert_eq!(cli.args.command, ["/probe", "--guest-option"]);
        for (option, value) in [
            ("--cpus", "0"),
            ("--cpus", "65"),
            ("--memory", "0"),
            ("--memory", "65537"),
        ] {
            assert!(
                Cli::try_parse_from(["vm-run", "--rootfs", "root", option, value, "--", "/probe"])
                    .is_err()
            );
        }
        assert!(Cli::try_parse_from(["vm-run", "--rootfs", "root"]).is_err());
    }
}

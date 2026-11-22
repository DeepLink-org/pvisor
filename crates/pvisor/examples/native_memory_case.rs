//! Linux/KVM driver for pvisor-kernel/tests/memory.py.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux {
    use anyhow::{Context, ensure};
    use clap::{Parser, Subcommand};
    use pvisor::ram_backing::{
        ipc::{PoolClient, serve_shared},
        shared::SharedPool,
    };
    use pvisor_vm::api::{
        ColdRamControl, ColdRamOptions, RamDedupControl, VmBuilder, VmConfiguration, VmControl,
        VmRuntime,
    };
    use std::{
        fs::File,
        io::{self, ErrorKind},
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    #[derive(Subcommand)]
    enum Case {
        Pool { socket: PathBuf },
        Physical { root: PathBuf, socket: PathBuf },
        Ksm { root: PathBuf },
        Offload { root: PathBuf, backing: PathBuf },
        Unsupported,
    }

    #[derive(Parser)]
    struct Args {
        /// Explicit firmware for Linux; omit for the embedded Rust native guest.
        #[arg(long)]
        firmware: Option<PathBuf>,
        #[command(subcommand)]
        case: Case,
    }

    pub(super) fn run() -> anyhow::Result<()> {
        let Args { firmware, case } = Args::parse();
        if let Case::Pool { socket } = case {
            let pool = Arc::new(Mutex::new(SharedPool::new(64 << 20, 16384)?));
            let listener = UnixListener::bind(socket)?;
            for _ in 0..2 {
                let (peer, _) = listener.accept()?;
                let pool = pool.clone();
                std::thread::spawn(move || serve_shared(peer, pool, 65536));
            }
            // The supervisor owns the deadline and reaps this server after both VMs.
            std::thread::park();
            anyhow::bail!("pool server was unparked");
        }

        let mut vm = VmBuilder::new(2, 256)?;
        if let Some(path) = firmware {
            vm.set_firmware_path(path.canonicalize()?)?;
        }
        let root = match &case {
            Case::Physical { root, .. } | Case::Ksm { root } | Case::Offload { root, .. } => {
                root.canonicalize()?
            }
            Case::Unsupported => {
                ensure!(vm.guest_idle_handoff().unwrap_err().kind() == ErrorKind::Unsupported);
                ensure!(vm.snapshot_profile().unwrap_err().kind() == ErrorKind::Unsupported);
                return Ok(());
            }
            Case::Pool { .. } => unreachable!(),
        };
        ensure!(
            root.parent().is_some(),
            "host root cannot be used as rootfs"
        );
        if let Case::Offload { backing, .. } = &case {
            vm.ram_backing(
                File::options()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(backing)?,
            )?;
        }
        vm.filesystem("/dev/root", &root, 0)?;
        vm.task_stdio(0, 1, 2)?;
        let launch = pvisor_guest::api::GuestConfig {
            argv: vec!["/probe".into()],
            cwd: "/".into(),
            stdio_ports: Some([true; 3]),
            ..Default::default()
        };
        vm.virtual_file(
            "/dev/root",
            pvisor_guest::api::CONFIG_PATH,
            serde_json::to_vec(&launch)?,
            0o400,
            true,
        )?;
        vm.run(move |handle| match case {
            Case::Physical { socket, .. } => {
                let mut pool =
                    PoolClient::new(UnixStream::connect(socket)?, Duration::from_secs(5))?;
                pool.enable_shared_mapping()?;
                assert!(handle.is_paused().map_err(io::Error::other)?);
                handle.start_cold_pager(
                    pool,
                    ColdRamOptions {
                        page_granular: true,
                        share_resident: true,
                        ..Default::default()
                    },
                )?;
                assert!(handle.is_paused().map_err(io::Error::other)?);
                Ok(())
            }
            Case::Ksm { .. } => {
                let report = handle.advise_ram_dedup().map_err(io::Error::other)?;
                if report.accepted_bytes == 0 {
                    return Err(io::Error::other(format!("no RAM accepted: {report:?}")));
                }
                eprintln!("KSM accepted {} bytes", report.accepted_bytes);
                Ok(())
            }
            Case::Offload { .. } => {
                std::thread::spawn(move || {
                    if let Err(error) = offload(&root, &handle) {
                        eprintln!("{error:#}");
                        std::process::exit(1);
                    }
                });
                Ok(())
            }
            _ => unreachable!(),
        })?;
        anyhow::bail!("VM returned without an authenticated exit")
    }

    fn offload(root: &std::path::Path, handle: &impl VmControl) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("ready").try_exists()? {
            ensure!(Instant::now() < deadline, "guest did not populate its RAM");
            std::thread::sleep(Duration::from_millis(20));
        }
        let report = handle.offload_ram().map_err(io::Error::other)?;
        let before = report
            .resident_before_bytes
            .context("missing residency before offload")?;
        let after = report
            .resident_after_bytes
            .context("missing residency after offload")?;
        eprintln!("offload: {report:?}");
        ensure!(
            report.backed_bytes > 0 && before > after,
            "RAM was not reclaimed"
        );
        ensure!(handle.is_paused().map_err(io::Error::other)?);
        handle.resume().map_err(io::Error::other)?;
        std::fs::write(root.join("resumed"), b"")?;
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("requires Linux x86_64/KVM")
}

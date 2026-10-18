//! Native helper for B-MEMORY-SCALE's Firecracker/pVisor curve comparison.
//! One VM per process, one common static guest worker, parent-owned budgets.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod native {
    use anyhow::{Context, ensure};
    use clap::{Parser, ValueEnum};
    use pvisor::ram_backing::{
        ipc::{PoolClient, SealedClient},
        resident::LocalColdRamStore,
    };
    use pvisor_vm::api::{
        ColdRamControl, ColdRamOptions, OverlayConfig, PermissionSemantics, RamDedupControl,
        VmBuilder, VmConfiguration, VmRuntime,
    };
    use std::{os::unix::net::UnixStream, path::PathBuf, time::Duration};

    #[derive(Clone, Copy, ValueEnum)]
    enum Mode {
        Off,
        Ksm,
        Physical,
        Compressed,
        Local,
    }

    #[derive(Parser)]
    struct Args {
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long)]
        worker: PathBuf,
        #[arg(long)]
        stage: PathBuf,
        #[arg(long, value_enum)]
        mode: Mode,
        #[arg(long)]
        pool_socket: Option<PathBuf>,
        #[arg(long)]
        pattern: String,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=4))]
        instance: u8,
        #[arg(long)]
        metrics: bool,
        /// Caller-owned immutable directory containing libpayload.so.
        #[arg(long)]
        libraries: Option<PathBuf>,
        /// Same immutable attachment and window, guest uses dax=never.
        #[arg(long, requires = "libraries")]
        buffered_libraries: bool,
        /// Load the payload through ordinary rootfs virtio-fs READs.
        #[arg(long, conflicts_with = "libraries")]
        rootfs_library: bool,
    }

    pub(super) fn run() -> anyhow::Result<()> {
        let a = Args::parse();
        ensure!(
            matches!(
                a.pattern.as_str(),
                "repeated" | "random-shared" | "random-unique"
            ),
            "unknown payload pattern"
        );
        ensure!(
            a.pool_socket.is_some() == matches!(a.mode, Mode::Physical | Mode::Compressed),
            "pool socket and mode disagree"
        );
        ensure!(
            (!a.rootfs_library && a.libraries.is_none()) || a.pattern == "random-shared",
            "library payload requires random-shared"
        );
        std::fs::create_dir(&a.stage).context("stage must be a NEW directory")?;
        let upper = a.stage.join("upper");
        let work = a.stage.join("work");
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        let mut vm = VmBuilder::new(1, 256)?;
        vm.set_firmware_path(a.firmware.canonicalize()?)?;
        vm.overlay(
            "/dev/root",
            OverlayConfig {
                lower_dirs: vec![a.rootfs.canonicalize()?],
                lower_mutability: vec![],
                upper_dir: upper,
                work_dir: Some(work),
                preimage_dir: None,
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            },
            0,
        )?;
        vm.virtual_file(
            "/dev/root",
            "/bench-worker",
            std::fs::read(&a.worker)?,
            0o500,
            a.libraries.is_none() && !a.rootfs_library,
        )?;
        let argv = if let Some(libraries) = &a.libraries {
            ensure!(
                matches!(a.mode, Mode::Off),
                "library data-path comparison requires mode off"
            );
            vm.immutable_filesystem("bench-libraries", &libraries.canonicalize()?, 128 << 20)?;
            vec![
                "/usr/bin/python3".to_string(),
                "-B".into(),
                "-u".into(),
                "/bench-worker".into(),
                a.pattern.clone(),
                a.instance.to_string(),
                if a.buffered_libraries {
                    "buffered"
                } else {
                    "dax"
                }
                .into(),
            ]
        } else if a.rootfs_library {
            vec![
                "/usr/bin/python3".into(),
                "-B".into(),
                "-u".into(),
                "/bench-worker".into(),
                a.pattern.clone(),
                a.instance.to_string(),
                "rootfs".into(),
            ]
        } else {
            vec![
                "/bench-worker".into(),
                "--native".into(),
                a.pattern.clone(),
                a.instance.to_string(),
            ]
        };
        let launch = serde_json::json!({
            "argv":argv,
            "env":{}, "cwd":"/", "stdio_ports":[true,true,true]
        });
        vm.virtual_file(
            "/dev/root",
            "/.pvisor-guest.json",
            serde_json::to_vec(&launch)?,
            0o400,
            true,
        )?;
        vm.run(move |handle| {
            let options = |physical| ColdRamOptions {
                metrics: a.metrics,
                page_granular: physical,
                share_resident: physical,
            };
            match a.mode {
                Mode::Off => Ok(()),
                Mode::Ksm => {
                    let report = handle.advise_ram_dedup().map_err(std::io::Error::other)?;
                    if report.accepted_bytes == 0 {
                        return Err(std::io::Error::other("KSM accepted no guest RAM"));
                    }
                    eprintln!("pool-case-ksm accepted_bytes={}", report.accepted_bytes);
                    Ok(())
                }
                Mode::Local => {
                    handle.start_cold_pager(LocalColdRamStore::new(128 << 20, 4096), options(false))
                }
                Mode::Physical | Mode::Compressed => {
                    let socket = a.pool_socket.as_ref().unwrap();
                    let mut pool =
                        PoolClient::new(UnixStream::connect(socket)?, Duration::from_secs(5))?;
                    if matches!(a.mode, Mode::Physical) {
                        pool.enable_shared_mapping()?;
                        handle.start_cold_pager(pool, options(true))
                    } else {
                        handle.start_cold_pager(SealedClient::new(pool)?, options(false))
                    }
                }
            }
        })?;
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() -> anyhow::Result<()> {
    native::run()
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() -> anyhow::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "pool_memory_case requires Linux x86_64/KVM",
    )
    .into())
}

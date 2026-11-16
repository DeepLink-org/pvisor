//! B-MEMORY-SCALE native VM owner for static and dlopen fixtures.
//! Built outside measurement; one invocation owns one fresh VM and overlay.
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
        #[arg(long,value_parser=clap::value_parser!(u8).range(1..=4))]
        instance: u8,
        #[arg(long)]
        libraries: Option<PathBuf>,
        #[arg(long, conflicts_with = "libraries")]
        rootfs_library: bool,
    }
    pub fn run() -> anyhow::Result<()> {
        let args = Args::parse();
        ensure!(
            ["repeated", "random-shared", "random-unique"].contains(&args.pattern.as_str()),
            "invalid pattern"
        );
        ensure!(
            args.pool_socket.is_some() == matches!(args.mode, Mode::Physical | Mode::Compressed),
            "pool mode/socket mismatch"
        );
        ensure!(
            (!args.rootfs_library && args.libraries.is_none()) || args.pattern == "random-shared",
            "library requires shared-random oracle"
        );
        std::fs::create_dir(&args.stage).context("VM stage must be NEW")?;
        let upper = args.stage.join("upper");
        let work = args.stage.join("work");
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        let mut vm = VmBuilder::new(1, 256)?;
        vm.set_firmware_path(args.firmware.canonicalize()?)?;
        vm.overlay(
            "/dev/root",
            OverlayConfig {
                lower_dirs: vec![args.rootfs.canonicalize()?],
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
        // Executable native bytes, not a Python script or injected measurement
        // policy. Library mode retains ordinary mutable-rootfs qualification.
        vm.virtual_file(
            "/dev/root",
            "/bench-worker",
            std::fs::read(&args.worker)?,
            0o500,
            args.libraries.is_none() && !args.rootfs_library,
        )?;
        let mut argv = vec![
            "/bench-worker".to_string(),
            "--native".into(),
            args.pattern.clone(),
            args.instance.to_string(),
        ];
        if let Some(libraries) = &args.libraries {
            ensure!(
                matches!(args.mode, Mode::Off),
                "DAX reference uses mode off"
            );
            vm.immutable_filesystem("bench-libraries", &libraries.canonicalize()?, 128 << 20)?;
            argv.push("dax".into());
        } else if args.rootfs_library {
            argv.push("rootfs".into());
        }
        vm.virtual_file(
            "/dev/root",
            "/.pvisor-guest.json",
            serde_json::to_vec(&serde_json::json!({
                "argv":argv,"env":{},"cwd":"/","stdio_ports":[true,true,true]
            }))?,
            0o400,
            true,
        )?;
        vm.run(move |handle| {
            let options = |physical| ColdRamOptions {
                metrics: false,
                page_granular: physical,
                share_resident: physical,
            };
            match args.mode {
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
                    let mut pool = PoolClient::new(
                        UnixStream::connect(args.pool_socket.as_ref().unwrap())?,
                        Duration::from_secs(5),
                    )?;
                    if matches!(args.mode, Mode::Physical) {
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
    anyhow::bail!("memory VM worker requires Linux x86_64/KVM")
}

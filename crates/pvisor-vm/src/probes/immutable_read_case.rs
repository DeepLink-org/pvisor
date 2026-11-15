//! KVM correctness gate, not a benchmark. Owns all guest processes/directories.
//! Usage: immutable_read_case STATIC_GUEST KERNEL_BUNDLE PHYSICAL_POOL_SOCKET
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod native {
    use pvisor::ram_backing::ipc::{PhysicalClient, PoolClient};
    use pvisor_vm::api::{
        ColdRamControl, ColdRamOptions, OverlayConfig, PermissionSemantics, VmBuilder,
        VmConfiguration, VmRuntime,
    };
    use std::{
        fs,
        os::unix::net::UnixStream,
        path::PathBuf,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };
    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    struct Guest(Child);
    impl Drop for Guest {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(super) fn run() -> Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() == 6 && args[0] == "--runner" {
            let root = PathBuf::from(&args[5]);
            let mode = args[4].to_str().ok_or("invalid mode")?;
            let lower = root.join("lower");
            let mut vm = VmBuilder::new(1, 256)?;
            let bundle = PathBuf::from(&args[2]);
            let metadata: serde_json::Value =
                serde_json::from_slice(&fs::read(bundle.join("kernel.json"))?)?;
            vm.embedded_kernel(
                &fs::read(bundle.join("kernel.bin"))?,
                metadata["guest_addr"]
                    .as_u64()
                    .ok_or("missing load address")?,
                metadata["entry_addr"]
                    .as_u64()
                    .ok_or("missing entry address")?,
            )?;
            vm.overlay(
                "/dev/root",
                OverlayConfig {
                    lower_dirs: vec![lower.clone()],
                    lower_mutability: vec![pvisor_overlay_core::LayerMutability::Immutable],
                    upper_dir: root.join("upper"),
                    work_dir: Some(root.join("work")),
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
            if mode == "dax" {
                vm.immutable_filesystem("files", &lower, 2 << 20)?;
            }
            vm.task_stdio(-1, 1, 2)?;
            vm.virtual_file(
                "/dev/root",
                "/file-probe",
                fs::read(&args[1])?,
                0o500,
                false,
            )?;
            let config = pvisor_guest::GuestConfig {
                argv: vec!["/file-probe".into(), mode.into()],
                cwd: "/".into(),
                stdio_ports: Some([false, true, true]),
                ..Default::default()
            };
            vm.virtual_file(
                "/dev/root",
                pvisor_guest::CONFIG_PATH,
                serde_json::to_vec(&config)?,
                0o400,
                true,
            )?;
            let socket = PathBuf::from(&args[3]);
            vm.run(move |handle| {
                let stream = UnixStream::connect(&socket)?;
                pvisor::host_transport::authorize_host_peer_sync(&stream)
                    .map_err(std::io::Error::other)?;
                let mut client = PoolClient::new(stream, Duration::from_secs(5))?;
                client.enable_shared_mapping()?;
                handle.start_cold_pager(
                    PhysicalClient::new(client, Some(socket))?,
                    ColdRamOptions {
                        metrics: true,
                        page_granular: true,
                        share_resident: true,
                    },
                )
            })?;
            return Err("VMM returned without authenticated guest exit".into());
        }
        if args.len() != 3 {
            return Err("expected STATIC_GUEST KERNEL_BUNDLE PHYSICAL_POOL_SOCKET".into());
        }
        for mode in ["native", "disabled", "dax"] {
            let root = tempfile::tempdir()?;
            for name in ["lower", "upper", "work"] {
                fs::create_dir(root.path().join(name))?;
            }
            let expected = (0..4 << 20)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>();
            fs::write(root.path().join("lower/payload"), &expected)?;
            let output = root.path().join("stdout");
            let error = root.path().join("stderr");
            let mut child = Guest(
                Command::new(std::env::current_exe()?)
                    .arg("--runner")
                    .args(&args)
                    .arg(mode)
                    .arg(root.path())
                    .env(
                        "PVISOR_MEMORY_FILE_HINTS",
                        if mode == "disabled" { "0" } else { "1" },
                    )
                    .stdin(Stdio::null())
                    .stdout(fs::File::create(&output)?)
                    .stderr(fs::File::create(&error)?)
                    .spawn()?,
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            let status = loop {
                if let Some(status) = child.0.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    return Err(format!("{mode}: VM timed out").into());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let stdout = fs::read_to_string(&output)?;
            let stderr = fs::read_to_string(&error)?;
            if !status.success() || !stdout.contains("IMMUTABLE_READ_GUEST_PASS") {
                return Err(format!("{mode}: {status}\n{stdout}\n{stderr}").into());
            }
            let mapped = stderr.contains("pvisor-cold-file mapped_bytes_total=");
            if mapped != (mode == "native") {
                return Err(format!("{mode}: wrong native file path engagement\n{stderr}").into());
            }
            if fs::read(root.path().join("lower/payload"))? != expected {
                return Err(format!("{mode}: private guest writes changed host backing").into());
            }
            println!("IMMUTABLE_READ_CASE mode={mode} PASS");
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return native::run();
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    Err("immutable READ sharing requires Linux x86_64/KVM".into())
}

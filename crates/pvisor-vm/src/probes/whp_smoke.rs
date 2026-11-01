//! A real WHP test. Missing virtualization/firmware is failure, never a skip.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pvisor_vm::api::{
        RuntimeSupport, VmBuilder, VmConfiguration, VmControl, VmPlatform, VmRuntime,
    };
    use std::{
        io,
        path::PathBuf,
        process::Command,
        time::{Duration, Instant},
    };
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--runner") && args.len() == 7 {
        VmPlatform::init_logging("info");
        let root = PathBuf::from(&args[5]);
        let cpus = args[4].to_str().ok_or("CPU count")?.parse()?;
        let mut vm = VmBuilder::new(cpus, 256)?;
        vm.set_firmware_path(PathBuf::from(&args[1]).canonicalize()?)?;
        vm.filesystem("/dev/root", &root, 0)?;
        vm.read_only_filesystem("readonly", &PathBuf::from(&args[6]))?;
        vm.virtual_file(
            "/dev/root",
            "/whp-probe",
            std::fs::read(&args[2])?,
            0o755,
            false,
        )?;
        let config = pvisor_guest::GuestConfig {
            argv: vec![
                "/whp-probe".into(),
                args[3].to_str().ok_or("exit code")?.into(),
                cpus.to_string(),
            ],
            cwd: "/".into(),
            stdio_ports: Some([false; 3]),
            shares: vec![pvisor_guest::GuestShare {
                tag: "readonly".into(),
                path: "/readonly".into(),
                read_only: true,
            }],
            ..Default::default()
        };
        vm.virtual_file(
            "/dev/root",
            pvisor_guest::CONFIG_PATH,
            serde_json::to_vec(&config)?,
            0o400,
            true,
        )?;
        let mut saved = None;
        let abort = args[3] == "abort";
        let outcome = vm.run(|handle| {
            assert!(handle.is_paused().map_err(io::Error::other)?);
            handle.pause().map_err(io::Error::other)?;
            if abort {
                saved = Some(handle);
                return Err(io::Error::other("intentional ready callback rejection"));
            }
            handle.resume().map_err(io::Error::other)?;
            handle.pause().map_err(io::Error::other)?;
            assert!(handle.is_paused().map_err(io::Error::other)?);
            Ok(())
        });
        if abort {
            assert_eq!(
                outcome.unwrap_err().to_string(),
                "intentional ready callback rejection"
            );
            assert!(saved.unwrap().is_paused().is_err());
            return Ok(());
        }
        outcome?;
        return Err("VMM returned instead of reporting guest exit".into());
    }
    if args.len() != 2 {
        return Err("expected FIRMWARE_DLL STATIC_LINUX_PROBE".into());
    }
    for (cpus, code) in [(1, "abort"), (1, "0"), (2, "0"), (2, "7"), (1, "255")] {
        let root = tempfile::tempdir()?;
        let readonly = tempfile::tempdir()?;
        std::fs::write(root.path().join("input"), b"pvisor-whp-input")?;
        std::fs::write(readonly.path().join("fixture"), b"immutable")?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--runner")
            .args(&args)
            .arg(code)
            .arg(cpus.to_string())
            .arg(root.path())
            .arg(readonly.path())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("WHP cpus={cpus} exit={code} timed out").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let expected = if code == "abort" { 0 } else { code.parse()? };
        if status.code() != Some(expected) {
            let mut diagnostic = String::new();
            for name in [".pvisor-guest-error", "probe-error", "result"] {
                if let Ok(bytes) = std::fs::read(root.path().join(name)) {
                    diagnostic.push_str(&format!("{name}: {}; ", String::from_utf8_lossy(&bytes)));
                }
            }
            return Err(format!("WHP cpus={cpus} exit={code}: {status}; {diagnostic}").into());
        }
        if code != "abort" {
            assert_eq!(
                std::fs::read(root.path().join("result"))?,
                b"pvisor-whp-read-write-ok"
            );
        }
        assert_eq!(
            std::fs::read(readonly.path().join("fixture"))?,
            b"immutable"
        );
        println!("WHP_SMOKE cpus={cpus} exit={code} PASS");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    panic!("whp_smoke must run on Windows with Windows Hypervisor Platform enabled");
}

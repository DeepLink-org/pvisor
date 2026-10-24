//! Real guest root attempts to forge success, then exits with the requested code.
use pvisor_vm::api::{RuntimeSupport, VmBuilder, VmConfiguration, VmPlatform, VmRuntime};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--runner") && args.len() == 5 {
        VmPlatform::init_logging("info");
        let root = PathBuf::from(&args[4]);
        let mut vm = VmBuilder::new(1, 256)?;
        vm.set_firmware_path(PathBuf::from(&args[1]))?;
        vm.filesystem("/dev/root", &root, 0)?;
        vm.task_stdio(-1, 1, 2)?;
        vm.virtual_file(
            "/dev/root",
            "/exit-probe",
            std::fs::read(&args[2])?,
            0o755,
            false,
        )?;
        let config = pvisor_guest::GuestConfig {
            argv: vec![
                "/exit-probe".into(),
                args[3].to_str().ok_or("invalid code")?.into(),
            ],
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
        vm.run(|_| Ok(()))?;
        return Err("VMM returned without process exit".into());
    }
    if args.len() != 2 {
        return Err("expected FIRMWARE GUEST_PROBE".into());
    }
    for code in [0, 7, 255] {
        let root = tempfile::tempdir()?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--runner")
            .args(&args)
            .arg(code.to_string())
            .arg(root.path())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("VM exit case {code} timed out").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if status.code() != Some(code) {
            let error = std::fs::read_to_string(root.path().join(".pvisor-guest-error"))
                .unwrap_or_default();
            return Err(format!("VM exit case {code} returned {status}: {error}").into());
        }
        println!("VM_EXIT_CASE code={code} PASS");
    }
    Ok(())
}

//! B-ALLOCATOR-RECLAIM-ENG worker; one fresh VMM process, no pager/offload.
use pvisor_vm::api::{RuntimeSupport, VmBuilder, VmConfiguration, VmPlatform, VmRuntime};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err("expected ROOT FIRMWARE PAYLOAD SO CONTROL_SOCKET".into());
    }
    VmPlatform::init_logging("info");
    eprintln!("allocator-vmm pid={}", std::process::id());
    let mut vm = VmBuilder::new(2, 512)?;
    vm.set_firmware_path(PathBuf::from(&args[1]))?;
    vm.filesystem("/dev/root", &PathBuf::from(&args[0]), 0)?;
    vm.task_stdio(-1, 1, 2)?;
    vm.virtual_file("/dev/root", "/payload", fs::read(&args[2])?, 0o555, false)?;
    vm.virtual_file(
        "/dev/root",
        "/reclaim.so",
        fs::read(&args[3])?,
        0o444,
        false,
    )?;
    let config = br#"{"argv":["/payload","--idle"],"env":{},"cwd":"/","control":true,"stdio_ports":[false,true,true],"process":{"new_session":true,"stdin":"closed","allocator_reclaim":"/reclaim.so"}}"#;
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        config.to_vec(),
        0o400,
        true,
    )?;
    vm.vsock_port(1025, PathBuf::from(&args[4]), true)?;
    vm.run(|_| Ok(()))?;
    Err("VM returned without authenticated guest exit".into())
}

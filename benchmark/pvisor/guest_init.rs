//! Paired init benchmark driver; both cases use the public VM contract.
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn main() -> anyhow::Result<()> {
    use pvisor_overlaynet::{
        BandwidthRegistry, EgressContext, EgressRuntime, NetworkConfig, NetworkPolicy,
    };
    use pvisor_vm::api::{GuestCommand, NetworkOptions, VmBuilder, VmConfiguration, VmRuntime};
    use std::{path::Path, sync::Arc};

    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 8,
        "expected init mode network rootfs init-file launch-file workspace"
    );
    let old = args[1] == "c";
    let workspace = args[2] == "workspace";
    let network = args[3] == "network";
    let init = std::fs::read(&args[5])?;
    let launch = std::fs::read(&args[6])?;
    let mut vm = VmBuilder::new(1, 128)?;
    vm.disable_implicit_init()?;
    // Preserve the old add_virtiofs API's default 512 MiB DAX window.
    vm.filesystem("/dev/root", Path::new(&args[4]), 1 << 29)?;
    vm.virtual_file("/dev/root", "/init.krun", init, 0o755, true)?;
    if !old {
        vm.virtual_file("/dev/root", "/.pvisor-guest.json", launch, 0o400, true)?;
    }
    if workspace {
        vm.filesystem("pvisor-workspace", Path::new(&args[7]), 1 << 29)?;
    }
    if old {
        vm.guest_command(GuestCommand {
            executable: if workspace {
                "/.pvisor-exec-bench.sh"
            } else {
                "/payload"
            }
            .into(),
            arguments: Vec::new(),
            environment: Default::default(),
            working_directory: "/".into(),
        })?;
    }
    let _network = if network {
        let policy = NetworkPolicy::compile(&NetworkConfig::default())?;
        let egress = EgressRuntime::with_bandwidth_registry(
            policy,
            Arc::new(pvisor_core::PolicyControlController),
            BandwidthRegistry::default(),
        );
        let config = pvisor_overlaynet::vm::VmNetworkConfig::new(egress, EgressContext::default());
        let (backend, peer) = pvisor_overlaynet::vm::VmNetwork::start(config)?;
        vm.network_with_options(
            peer,
            pvisor_overlaynet::vm::VM_MAC,
            NetworkOptions { guest_dhcp: old },
        )?;
        Some(backend)
    } else {
        None
    };
    // The runtime always installs zero-feature vsock; TSI stays disabled.
    vm.run(|_| Ok(()))?;
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn main() {
    eprintln!("VM init benchmark requires a supported VM target");
    std::process::exit(1);
}

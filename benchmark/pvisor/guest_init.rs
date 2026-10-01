//! Paired init benchmark driver; built by guest_init.py against identical host crates.
use pvisor_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, NetworkConfig, NetworkPolicy,
};
use std::{ffi::CString, os::fd::AsRawFd, path::Path, sync::Arc};

fn cpath(path: &str) -> CString {
    CString::new(path).unwrap()
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let old = args[1] == "c";
    let workspace = args[2] == "workspace";
    let network = args[3] == "network";
    let root = cpath(&args[4]);
    let init: &[u8] = if old {
        include_bytes!(env!("PVISOR_BENCH_C_INIT"))
    } else {
        include_bytes!(env!("PVISOR_BENCH_RUST_INIT"))
    };
    let launch = std::fs::read(&args[6]).unwrap();
    let ctx = krun::krun_create_ctx();
    assert!(ctx >= 0);
    let ctx = ctx as u32;
    assert_eq!(krun::krun_set_vm_config(ctx, 1, 128), 0);
    assert_eq!(krun::krun_disable_implicit_init(ctx), 0);
    unsafe {
        assert_eq!(
            krun::krun_add_virtiofs(ctx, c"/dev/root".as_ptr(), root.as_ptr()),
            0
        );
        assert_eq!(
            krun::krun_fs_add_overlay_file(
                ctx,
                c"/dev/root".as_ptr(),
                c"/init.krun".as_ptr(),
                init.as_ptr(),
                init.len(),
                0o755,
                true
            ),
            0
        );
        if !old {
            assert_eq!(
                krun::krun_fs_add_overlay_file(
                    ctx,
                    c"/dev/root".as_ptr(),
                    c"/.pvisor-guest.json".as_ptr(),
                    launch.as_ptr(),
                    launch.len(),
                    0o400,
                    true
                ),
                0
            );
        }
        if workspace {
            let path = cpath(&args[7]);
            assert_eq!(
                krun::krun_add_virtiofs(ctx, c"pvisor-workspace".as_ptr(), path.as_ptr()),
                0
            );
        }
        if old {
            let program = if args[2] == "direct" {
                c"/payload"
            } else {
                c"/.pvisor-exec-bench.sh"
            };
            // Match the former pVisor runner's empty argv/env kernel-command-line contract.
            let empty = [std::ptr::null(); 4096];
            assert_eq!(
                krun::krun_set_exec(ctx, program.as_ptr(), empty.as_ptr(), empty.as_ptr()),
                0
            );
            assert_eq!(krun::krun_set_workdir(ctx, c"/".as_ptr()), 0);
        }
    }
    let _network = if network {
        let policy = NetworkPolicy::compile(&NetworkConfig::default()).unwrap();
        let egress = EgressRuntime::with_bandwidth_registry(
            policy,
            Arc::new(pvisor_core::PolicyControlController),
            BandwidthRegistry::default(),
        );
        let config =
            pvisor_overlaynet::vm::VmNetworkConfig::new(egress, EgressContext::default());
        let (backend, peer) = pvisor_overlaynet::vm::VmNetwork::start(config).unwrap();
        unsafe {
            assert_eq!(
                krun::krun_add_net_unixstream(
                    ctx,
                    std::ptr::null(),
                    peer.as_raw_fd(),
                    pvisor_overlaynet::vm::VM_MAC.as_ptr(),
                    0,
                    if old { 2 } else { 0 }
                ),
                0
            );
        }
        Some((backend, peer))
    } else {
        None
    };
    assert_eq!(krun::krun_disable_implicit_vsock(ctx), 0);
    assert_eq!(krun::krun_add_vsock(ctx, 0), 0);
    let result = krun::krun_start_enter(ctx);
    panic!(
        "VMM returned {result}, root={}",
        Path::new(&args[4]).display()
    );
}

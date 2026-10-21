//! Keep the optional control transport from becoming a second guest runtime.
#[test]
fn jobs_and_controlled_workloads_share_launch_and_reaping() {
    let boot = include_str!("../src/linux.rs");
    let control = include_str!("../src/linux/control.rs");
    assert!(!boot.contains("container::run"));
    assert!(!boot.contains("config.container"));
    for source in [boot, control] {
        for private_operation in [
            "libc::waitpid",
            "libc::waitid",
            "libc::kill",
            "libc::SYS_prlimit64",
        ] {
            assert!(
                !source.contains(private_operation),
                "supervision escaped the common launcher: {private_operation}"
            );
        }
    }
    assert!(!control.contains("Command::"));
    assert!(!control.contains("libc::setsid"));
    assert!(!control.contains("Docker"));
}

#[test]
fn shim_uses_the_shared_guest_and_host_client() {
    let shim_vm = include_str!("../../pvisor-shim/src/vm.rs");
    let shim_service = include_str!("../../pvisor-shim/src/service.rs");
    let shim_entry = include_str!("../../pvisor-shim/src/main.rs");
    assert!(!shim_vm.contains("AGENT_GUEST_PATH"));
    assert!(!shim_entry.contains("run_guest_agent_if_requested"));
    assert!(shim_service.contains("ExecSession::start"));
    assert!(!shim_service.contains("FrameReader"));
    assert!(!shim_service.contains("FrameWriter"));
    assert!(!shim_service.contains("spawn_vm_exec_relay"));
}

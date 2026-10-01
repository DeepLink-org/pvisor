fn main() {
    #[cfg(target_os = "linux")]
    {
        // The shim binary is also the container init parent: when containerd
        // asks it to create a task it re-executes itself in internal mode
        // (house "self-exec" pattern, cf. pvisor's INTERNAL_SANDBOX_ARG).
        if let Err(error) = pvisor_shim::agent::run_guest_agent_if_requested() {
            eprintln!("pvisor shim guest agent failed: {error:#}");
            std::process::exit(1);
        }
        if let Err(error) = pvisor_shim::child::run_internal_if_requested() {
            eprintln!("pvisor shim internal init failed: {error:#}");
            std::process::exit(1);
        }
        pvisor_shim::service::shim_main();
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("containerd-shim-pvisor-v2 only supports Linux hosts");
        std::process::exit(1);
    }
}

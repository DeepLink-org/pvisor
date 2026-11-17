fn main() {
    #[cfg(target_os = "linux")]
    {
        // The shim re-executes itself in internal mode as the container init parent.
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

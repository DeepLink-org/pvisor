const MANIFEST: &str =
    persisting_pvisor::command_manifest!("cache", "Serve or query the shared OCI file cache");
fn main() -> anyhow::Result<()> {
    if persisting_pvisor::cli::extensions::manifest_requested(MANIFEST)? {
        return Ok(());
    }
    persisting_pvisor::cli::cache_main()
}

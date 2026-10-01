const MANIFEST: &str = persisting_pvisor::command_manifest!(
    "tui",
    "Run a Job in an interactive terminal with policy review"
);
fn main() -> anyhow::Result<()> {
    if persisting_pvisor::cli::extensions::manifest_requested(MANIFEST)? {
        return Ok(());
    }
    persisting_pvisor::cli::tui_main()
}

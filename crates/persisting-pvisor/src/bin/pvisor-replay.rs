const MANIFEST: &str = persisting_pvisor::command_manifest!(
    "replay",
    "Replay an agent-native trajectory and continue execution"
);
fn main() -> anyhow::Result<()> {
    if persisting_pvisor::cli::extensions::manifest_requested(MANIFEST)? {
        return Ok(());
    }
    persisting_pvisor::cli::replay_main()
}

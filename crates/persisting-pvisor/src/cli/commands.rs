//! Independent tool frontends and internal runtime entry points.
use super::{extensions, replay, run, terminal, tui};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "pvisor-replay",
    version,
    about = "Replay an agent-native trajectory"
)]
pub(crate) struct ReplayCli {
    #[command(flatten)]
    args: replay::ReplayArgs,
}

pub fn replay_main() -> anyhow::Result<()> {
    #[cfg(unix)]
    terminal::init_child_context();
    super::finish(replay::run(ReplayCli::parse().args));
    Ok(())
}

#[cfg(unix)]
pub fn tui_main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-tui",
        version,
        about = "Run a Job in an interactive terminal"
    )]
    struct TuiCli {
        #[command(flatten)]
        run: run::RunArgs,
    }
    terminal::init_child_context();
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "run") {
        args.remove(1);
    }
    let mut parsed = TuiCli::parse_from(&args);
    parsed.run.enable_tui();
    let audit = parsed.run.audit_requested()?;
    anyhow::ensure!(
        parsed.run.wants_tui(audit),
        "TUI requires inherited stdio and a normal Job"
    );
    anyhow::ensure!(
        terminal::available(),
        "TUI requires an interactive terminal"
    );
    args[0] = extensions::core_executable()?.into_os_string();
    args.insert(1, "run".into());
    super::finish(tui::run(args, audit)?);
    Ok(())
}

pub fn cache_main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-cache",
        version,
        about = "Serve or query the shared OCI file cache"
    )]
    struct CacheCli {
        #[command(flatten)]
        args: crate::image::cache::CacheArgs,
    }
    crate::image::cache::run(CacheCli::parse().args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_frontend_keeps_phase_flags_and_their_exclusivity() {
        for mode in ["--prepare-only", "--replay-only"] {
            ReplayCli::try_parse_from(["pvisor-replay", mode]).unwrap();
        }
        assert!(
            ReplayCli::try_parse_from(["pvisor-replay", "--prepare-only", "--replay-only"])
                .is_err()
        );
        let help = ReplayCli::try_parse_from(["pvisor-replay", "--help"])
            .unwrap_err()
            .to_string();
        for flag in [
            "--prepare-only",
            "--replay-only",
            "--allow-stale-observations",
            "--boundary-user-prompt",
        ] {
            assert!(help.contains(flag));
        }
    }
}

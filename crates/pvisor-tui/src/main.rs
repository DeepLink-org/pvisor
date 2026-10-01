mod zellij;
use clap::Parser;
use pvisor::cli::{RunArgs, extensions, terminal};

fn main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-tui",
        version,
        about = "Run a Job in an interactive terminal"
    )]
    struct TuiCli {
        #[command(flatten)]
        run: RunArgs,
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
    let code = zellij::run(args, audit)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

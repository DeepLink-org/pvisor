#[path = "../tui/mod.rs"]
mod tui;
use clap::{Parser, Subcommand};
use pvisor_cli::cli::{ResumeArgs, RunArgs, terminal};
use pvisor_cli::companions;

fn main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-tui",
        version,
        about = "Run a Job in an interactive terminal"
    )]
    struct TuiCli {
        #[command(subcommand)]
        action: Action,
    }
    #[derive(Subcommand)]
    enum Action {
        Run(Box<RunArgs>),
        Resume(ResumeArgs),
    }
    terminal::init_child_context();
    let mut args: Vec<_> = std::env::args_os().collect();
    if !args
        .get(1)
        .is_some_and(|arg| arg == "run" || arg == "resume" || arg == "--version" || arg == "-V")
    {
        args.insert(1, "run".into());
    }
    let mut parsed = TuiCli::parse_from(&args);
    let audit = match &mut parsed.action {
        Action::Run(run) => {
            run.enable_tui();
            let audit = run.audit_requested()?;
            anyhow::ensure!(
                run.wants_tui(audit),
                "TUI requires inherited stdio and a normal Job"
            );
            audit
        }
        Action::Resume(_) => false,
    };
    anyhow::ensure!(
        terminal::available(),
        "TUI requires an interactive terminal"
    );
    args[0] = companions::core_executable()?.into_os_string();
    let code = tui::run(args, audit)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

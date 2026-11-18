//! Process contracts are exercised out of process, before any runtime exists.
use pvisor_cli::api::{Application, CommandLine};
#[cfg(unix)]
use std::process::Command;

#[test]
fn entries_have_uniform_process_signatures() {
    let _: fn() -> anyhow::Result<()> = Application::run;
    let _: fn() -> anyhow::Result<()> = Application::replay;
}

#[test]
#[cfg(unix)]
fn executable_entries_preserve_help_and_parser_exit_codes() -> anyhow::Result<()> {
    for (binary, name) in [
        (env!("CARGO_BIN_EXE_pvisor"), "pvisor"),
        (env!("CARGO_BIN_EXE_pvisor-replay"), "pvisor-replay"),
    ] {
        let output = Command::new(binary).arg("--help").output()?;
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let help = std::str::from_utf8(&output.stdout)?;
        assert!(help.contains(&format!("Usage: {name}")));

        let output = Command::new(binary).arg("--not-a-pvisor-option").output()?;
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(std::str::from_utf8(&output.stderr)?.contains("unexpected argument"));
    }
    Ok(())
}

#[test]
#[cfg(windows)]
fn replay_reports_unsupported_without_starting_work() {
    let error = Application::replay().unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Unsupported
    );
}

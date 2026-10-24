//! B-FS-TOOLS: resolve the product-selected workspace before the frozen workload.
//!
//! The CLI selects its staged view through cwd. Resolving that cwd here retains
//! the selected view while giving the frozen workflow's subprocesses absolute
//! paths. The original workload executable and operation sequence stay intact.
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

fn run() -> std::io::Result<()> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected CASE WORK_ROOT FIXTURE_ROOT ROUNDS RAM_MIB",
        ));
    }
    let cwd = std::env::current_dir()?;
    for index in [1, 2] {
        if Path::new(&args[index]).is_relative() {
            args[index] = cwd.join(&args[index]).into_os_string();
        }
    }
    Err(Command::new("/bench-artifacts/workload").args(args).exec())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("USER_WORKLOADS_CWD_ERROR: {error}");
        std::process::exit(1);
    }
}

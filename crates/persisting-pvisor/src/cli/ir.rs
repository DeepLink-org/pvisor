//! Read-only tools for inspecting the new core representation.
use anyhow::{Result, ensure};
use clap::{Args, Subcommand};
use persisting_control::ir::{Expression, MAX_TEXT_BYTES};
use std::{io::Read, path::PathBuf};

#[derive(Debug, Args)]
pub struct IrArgs {
    #[command(subcommand)]
    command: IrCommand,
}

#[derive(Debug, Subcommand)]
enum IrCommand {
    /// Validate syntax, operation contracts and ordered context chains.
    Check { path: PathBuf },
    /// Print canonical readable IR. Accepts IR text or structured JSON.
    Format { path: PathBuf },
    /// Export the same IR as structured JSON.
    Json { path: PathBuf },
}

pub fn run(args: IrArgs) -> Result<()> {
    let path = match &args.command {
        IrCommand::Check { path } | IrCommand::Format { path } | IrCommand::Json { path } => path,
    };
    let mut source = String::new();
    std::fs::File::open(path)?
        .take((MAX_TEXT_BYTES + 1) as u64)
        .read_to_string(&mut source)?;
    ensure!(source.len() <= MAX_TEXT_BYTES, "IR exceeds size limit");
    let expression: Expression = if source.trim_start().starts_with('{') {
        serde_json::from_str(&source)?
    } else {
        source.parse()?
    };
    expression.validate()?;
    match args.command {
        IrCommand::Check { .. } => {
            println!("valid: {}", expression.to_text()?)
        }
        IrCommand::Format { .. } => println!("{}", expression.to_text()?),
        IrCommand::Json { .. } => println!("{}", serde_json::to_string_pretty(&expression)?),
    }
    Ok(())
}

use std::{collections::BTreeSet, path::PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TraceArgs {
    #[command(subcommand)]
    command: TraceCommand,
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    /// Check event schemas, journal positions, identities and causal cycles.
    Check { path: PathBuf },
    /// Print operation-first pipeline expressions with their event phase.
    Show { path: PathBuf },
    /// Print records as readable JSON, including their typed core facts.
    Json { path: PathBuf },
}

pub fn run(args: TraceArgs) -> Result<()> {
    let path = match &args.command {
        TraceCommand::Check { path }
        | TraceCommand::Show { path }
        | TraceCommand::Json { path } => path,
    };
    let records = crate::trace::Journal::read(path)?;
    match args.command {
        TraceCommand::Check { .. } => {
            let ids: BTreeSet<_> = records.iter().map(|r| &r.event.id).collect();
            let unresolved: BTreeSet<_> = records
                .iter()
                .flat_map(|r| &r.event.caused_by)
                .filter(|id| !ids.contains(id))
                .collect();
            println!(
                "valid: {} records, {} unresolved causal references (execution completeness is not inferred)",
                records.len(),
                unresolved.len()
            );
        }
        TraceCommand::Show { .. } => {
            for record in &records {
                println!("{} {}", record.position.offset, record.event.to_text()?);
            }
        }
        TraceCommand::Json { .. } => println!("{}", serde_json::to_string_pretty(&records)?),
    }
    Ok(())
}

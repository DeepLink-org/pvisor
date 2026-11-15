//! Local volume commands; runtime owns references and deletion authority.
use super::*;
use query::Format;
use serde_json::json;

#[derive(Debug, Args)]
pub(in crate::cli) struct VolumeArgs {
    #[command(subcommand)]
    command: VolumeCommand,
}
#[derive(Debug, Subcommand)]
enum VolumeCommand {
    Create {
        #[arg(short = 'd', long, default_value = "local", value_parser = ["local"])]
        driver: String,
        name: Option<String>,
    },
    #[command(alias = "list")]
    Ls {
        #[arg(short, long)]
        quiet: bool,
        #[arg(long)]
        format: Option<String>,
    },
    Inspect {
        #[arg(short = 'f', long)]
        format: Option<String>,
        #[arg(required = true, num_args = 1.., value_name = "VOLUME")]
        volumes: Vec<String>,
    },
    #[command(alias = "remove")]
    Rm {
        #[arg(required = true, num_args = 1.., value_name = "VOLUME")]
        volumes: Vec<String>,
    },
}

pub(super) fn run(args: VolumeArgs) -> anyhow::Result<i32> {
    let format = match &args.command {
        VolumeCommand::Ls { format, .. } => format.as_deref(),
        VolumeCommand::Inspect { format, .. } => format.as_deref(),
        _ => None,
    }
    .map(Format::parse)
    .transpose()?;
    let store = Store::discover()?;
    match args.command {
        VolumeCommand::Create { name, .. } => {
            println!("{}", store.create_volume(name.as_deref())?.name)
        }
        VolumeCommand::Ls { quiet, .. } => {
            if !quiet {
                if let Some(format) = &format {
                    if let Some(header) = format.header() {
                        println!("{header}");
                    }
                } else {
                    println!("DRIVER\tVOLUME NAME");
                }
            }
            for volume in store.volumes()? {
                if quiet {
                    println!("{}", volume.name);
                } else if let Some(format) = &format {
                    println!(
                        "{}",
                        format.render(&json!({"Name":volume.name,"Driver":volume.driver}))?
                    );
                } else {
                    println!("{}\t{}", volume.driver, volume.name);
                }
            }
        }
        VolumeCommand::Inspect { volumes, .. } => {
            let mut rows = vec![];
            let status = batch(&volumes, |name| {
                let volume = store.inspect_volume(name)?;
                let row = json!({"Name":volume.name,"Driver":volume.driver,"Scope":"local","Labels":{},"Options":{}});
                if let Some(format) = &format {
                    println!("{}", format.render(&row)?);
                } else {
                    rows.push(row);
                }
                Ok(())
            })?;
            if format.is_none() {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
            return Ok(status);
        }
        VolumeCommand::Rm { volumes } => {
            return batch(&volumes, |name| {
                store.remove_volume(name)?;
                println!("{name}");
                Ok(())
            });
        }
    }
    Ok(0)
}

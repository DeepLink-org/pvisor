//! Project artifact export parsing and rendering; runtime owns validation and leases.
use clap::{ArgGroup, Args};
use pvisor::job_service::{ExportRequest, JobSelection, RuntimeJobService};
use std::path::PathBuf;

pub(super) const EXPORT_LONG_ABOUT: &str = "Export selected retained project changes without root privileges, consuming the stage, or applying anything to the project. JOB defaults to last in the current workspace. Linux only.\n\nThe output must be a new directory with an existing parent. Regular file contents go into DIR/files; manifest.json records the inventory, logical modes and deletions. Deletions are informational only. This is not a complete patch, merged snapshot or dependency closure: unselected files and experimental environment changes are not automatically accepted.\n\nRequires validated completion evidence and a retained stage. Live, unknown, applied or dropped stages are refused. Symlinks, special files, type replacements and opaque dependencies are unsupported. Inspect the manifest and deletions before manually accepting any results.";

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("selection").required(true).args(["paths", "all"])))]
pub(super) struct ExportArgs {
    /// Job id, stage directory, upper directory, or workspace path; defaults to last.
    #[arg(value_name = "JOB")]
    selector: Option<PathBuf>,
    /// New destination directory (absolute or relative); never replaced.
    #[arg(long, value_name = "DIR")]
    output: PathBuf,
    /// Job lookup storage, independent of the export destination.
    #[arg(long, value_name = "DIR", default_value = ".pvisor/capture")]
    storage: PathBuf,
    /// Export this project-relative file or subtree. Repeatable.
    #[arg(long = "path", value_name = "PROJECT_RELATIVE")]
    paths: Vec<PathBuf>,
    /// Explicitly select all retained project changes, not the whole rootfs.
    #[arg(long)]
    all: bool,
    /// Print the runtime export response as JSON, without human guidance.
    #[arg(long)]
    json: bool,
}

pub(super) fn export(args: ExportArgs) -> anyhow::Result<()> {
    let response = RuntimeJobService::export(
        &super::host::service_context(),
        ExportRequest {
            job: JobSelection {
                selector: args.selector,
                storage: args.storage,
            },
            output: args.output,
            paths: args.paths,
            all: args.all,
        },
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("job: {}", response.job_id);
        println!(
            "outcome: {:?}, exit code: {}",
            response.state,
            response
                .exit_code
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        );
        if response.partial_results {
            println!(
                "Warning: partial results may remain from a failed, cancelled or nonzero-exit task. Export success does not mean task success."
            );
        }
        println!("export: {}", response.destination.display());
        println!("project: {}", response.project_root.display());
        println!(
            "inventory: {} entries, {} deletions (manifest only)",
            response.entries.len(),
            response.deletions.len()
        );
        println!(
            "Next: inspect manifest.json, files/ and the deletions before manually accepting results."
        );
        println!(
            "Nothing was automatically applied; the stage is retained. Unexported files and experimental environment changes were not accepted. This is not a complete patch or dependency closure."
        );
    }
    Ok(())
}

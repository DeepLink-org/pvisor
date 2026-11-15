//! Project artifact export parsing and rendering; runtime owns validation and leases.
use clap::{ArgGroup, Args};
use pvisor::session_service::{ExportRequest, SessionSelection, SessionService};
use std::path::PathBuf;

pub(super) const EXPORT_LONG_ABOUT: &str = "Export a stopped Session/container merged rootfs as tar to stdout or -o/--output FILE. Attached volume contents are excluded; existing files are not replaced. Live/staged rootfs export and unsupported special files fail explicitly.\n\nNative project results: --path or --all explicitly selects retained project changes without consuming the stage or applying anything. SESSION defaults to last in the current workspace. This capability is Linux only; --output must name a new directory with an existing parent. Regular contents go into DIR/files; manifest.json records inventory, logical modes and deletions. Deletions are informational only, not an applied patch or a complete environment.\n\nProject-result export requires validated completion evidence and a retained stage. Live, unknown, applied or dropped stages are refused. Symlinks, special files, type replacements and opaque dependencies are unsupported for project results. Inspect manifest.json and deletions before accepting results.";

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("selection").args(["paths", "all"])))]
pub(super) struct ExportArgs {
    /// Session id, stage directory, upper directory, or workspace path; defaults to last.
    #[arg(value_name = "SESSION", required_unless_present = "selection")]
    selector: Option<PathBuf>,
    /// New destination directory (absolute or relative); never replaced.
    #[arg(long, short = 'o', value_name = "PATH")]
    output: Option<PathBuf>,
    /// Session lookup storage, independent of the export destination.
    #[arg(long, value_name = "DIR", default_value = ".pvisor/capture")]
    storage: PathBuf,
    /// Export this project-relative file or subtree. Repeatable.
    #[arg(long = "path", value_name = "PROJECT_RELATIVE", requires = "output")]
    paths: Vec<PathBuf>,
    /// Explicitly select all retained project changes, not the whole rootfs.
    #[arg(long, requires = "output")]
    all: bool,
    /// Print the runtime export response as JSON, without human guidance.
    #[arg(long, requires = "selection")]
    json: bool,
}

pub(super) fn export(args: ExportArgs) -> anyhow::Result<()> {
    if !args.all && args.paths.is_empty() {
        let selector = args
            .selector
            .as_ref()
            .and_then(|path| path.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("rootfs export requires a Session/container selector")
            })?;
        let store = pvisor::docker::Store::discover()?;
        if let Some(output) = args.output {
            // Spool before publishing: source/admission errors never leave an
            // empty destination. Existing files are never silently replaced.
            let mut spool = tempfile::tempfile()?;
            store.export_rootfs(selector, &mut spool)?;
            use std::io::Seek;
            spool.rewind()?;
            let mut destination = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            std::io::copy(&mut spool, &mut destination)?;
        } else {
            store.export_rootfs(selector, &mut std::io::stdout())?;
        }
        return Ok(());
    }
    let response = SessionService::export(
        &super::host::service_context(),
        ExportRequest {
            session: SessionSelection {
                selector: args.selector,
                storage: args.storage,
            },
            output: args
                .output
                .ok_or_else(|| anyhow::anyhow!("project export requires --output"))?,
            paths: args.paths,
            all: args.all,
        },
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("session: {}", response.session_id);
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

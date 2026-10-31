//! Resolve container stages without reinterpreting failed Job lookups as containers.
use anyhow::{Context, ensure};
use std::path::{Path, PathBuf};

pub(super) fn resolve(
    selector: Option<&Path>,
    storage: &Path,
    mount: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(selector) = selector else {
        ensure!(mount.is_none(), "--mount requires a container name/id");
        return Ok(None);
    };
    if mount.is_none() {
        // Paths and workspace aliases always retain Job semantics, including errors.
        if selector == Path::new("last") || selector.components().count() != 1 || present(selector)?
        {
            return Ok(Some(selector.to_path_buf()));
        }
        check_default_jobs(selector)?;
        match pvisor::resolve_run(Some(selector), storage) {
            Ok(_) => return Ok(Some(selector.to_path_buf())),
            Err(error) => {
                // resolve_run has no typed NotFound error. Only its exact terminal
                // ID-miss permits container lookup; index/read/ambiguity errors do not.
                let missing = format!("pVisor Run not found: {}", selector.display());
                if error.to_string() != missing || error.chain().count() != 1 {
                    return Err(error);
                }
            }
        }
    }
    let name = selector
        .to_str()
        .context("container selector must be UTF-8")?;
    let stage = pvisor::docker::Store::discover()?.staged_mount_stage(name, mount)?;
    Ok(Some(stage))
}

// Runtime ID discovery skips unreadable default records. Before allowing a
// container fallback, fail closed when those records cannot rule out a Job
// collision, and reject duplicate Job identities rather than choosing one.
fn present(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn check_default_jobs(selector: &Path) -> anyhow::Result<()> {
    let mut roots = vec![
        pvisor::default_run_home(),
        std::env::temp_dir().join("pvisor-runs"),
    ];
    roots.sort();
    roots.dedup();
    let encoded: String = selector
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut matching = std::collections::BTreeSet::new();
    for root in roots {
        if !present(&root)? {
            continue;
        }
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                continue;
            }
            let directory = entry.path();
            let metadata = directory.join("run.json");
            if present(&metadata)? {
                let record = pvisor::RunRecord::read(&directory)
                    .context("cannot exclude a corrupt default Job; use --mount to explicitly select a container")?;
                if Path::new(&record.run_id) == selector {
                    matching.insert(record.stage_dir());
                }
            }
            let index = directory
                .join(".pvisor/runs")
                .join(format!("{encoded}.json"));
            if present(&index)? {
                let record = pvisor::resolve_run(Some(selector), &directory)?;
                matching.insert(record.stage_dir());
            }
        }
    }
    ensure!(
        matching.len() <= 1,
        "ambiguous existing Job selector: {}",
        selector.display()
    );
    Ok(())
}

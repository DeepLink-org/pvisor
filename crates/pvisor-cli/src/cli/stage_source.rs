//! Resolve container stages without reinterpreting failed Session lookups as containers.
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
        // Paths and workspace aliases always retain Session semantics, including errors.
        if selector == Path::new("last") || selector.components().count() != 1 || present(selector)?
        {
            return Ok(Some(selector.to_path_buf()));
        }
        if let Some(record) = pvisor::lookup_run_id(selector, storage)? {
            return Ok(Some(record.stage_dir()));
        }
    }
    let name = selector
        .to_str()
        .context("container selector must be UTF-8")?;
    let stage = pvisor::docker::Store::discover()?.staged_mount_stage(name, mount)?;
    Ok(Some(stage))
}

fn present(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

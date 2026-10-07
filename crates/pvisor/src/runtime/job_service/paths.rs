use anyhow::Context;
use std::path::{Path, PathBuf};

/// Resolve existing symlink ancestors before validating a new branch path,
/// without creating directories inside the source Job on rejected requests.
pub(crate) fn fork_stage_candidate(path: &Path) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    fn resolve(path: &Path) -> anyhow::Result<PathBuf> {
        if path.try_exists()? {
            return Ok(path.canonicalize()?);
        }
        let name = path.file_name().context("invalid child stage path")?;
        let parent = path.parent().context("child stage has no parent")?;
        Ok(resolve(parent)?.join(name))
    }
    resolve(&path)
}

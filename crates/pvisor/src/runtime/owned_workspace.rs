//! Explicit ownership and admission for a new workspace over a sealed baseline.
use crate::environment_snapshot::SnapshotBase;
use crate::{OverlayHint, RunExecutor, VmExecutor};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::OpenOptionsExt};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const RECEIPT: &str = "workspace-baseline.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceBaseline {
    version: u32,
    workspace: crate::PreparedBase,
    rootfs: crate::PreparedBase,
    target: PathBuf,
}

pub(crate) struct ApplyWorkspaceOwner {
    _workspace: SnapshotBase,
    _rootfs: SnapshotBase,
}

pub(crate) struct OwnedWorkspace {
    pub(crate) base: SnapshotBase,
    pub(crate) target: PathBuf,
    workspace_store: PathBuf,
    rootfs_store: PathBuf,
    _rootfs: SnapshotBase,
    executor: Arc<dyn RunExecutor>,
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub(crate) fn candidate(path: &Path) -> anyhow::Result<PathBuf> {
    super::job_service::paths::fork_stage_candidate(path)
}

impl OwnedWorkspace {
    pub(crate) fn check_selector(
        &self,
        workspace: &crate::PreparedBase,
        rootfs: Option<&crate::PreparedRootfs>,
    ) -> anyhow::Result<()> {
        ensure!(
            workspace.store.canonicalize()? == self.workspace_store
                && workspace.base == *self.base.reference(),
            "managed workspace selector differs from the actual owned baseline"
        );
        let rootfs = rootfs
            .context("managed prepared workspace requires its actual prepared rootfs selector")?;
        ensure!(
            rootfs.store.canonicalize()? == self.rootfs_store
                && rootfs.base == *self._rootfs.reference(),
            "managed rootfs selector differs from the actual native owner"
        );
        Ok(())
    }
    pub(crate) fn new(
        base: SnapshotBase,
        target: PathBuf,
        executor: Arc<VmExecutor>,
    ) -> anyhow::Result<Self> {
        let (base, workspace_store) = base.workspace_owner()?;
        let (rootfs, rootfs_store) = executor.workspace_rootfs_owner(&workspace_store)?;
        ensure!(
            !overlap(&workspace_store, &rootfs_store),
            "rootfs and workspace stores must be independent trees"
        );
        let target = target
            .canonicalize()
            .context("resolve workspace apply target")?;
        ensure!(
            target.is_dir() && target != Path::new("/"),
            "owned workspace requires a directory apply target other than host root"
        );
        let owner = Self {
            base,
            target,
            workspace_store,
            rootfs_store,
            _rootfs: rootfs,
            executor,
        };
        owner.check_paths([owner.target.as_path()])?;
        Ok(owner)
    }

    pub(crate) fn executor(&self) -> Arc<dyn RunExecutor> {
        Arc::clone(&self.executor)
    }

    pub(crate) fn check_executor(&self, executor: &Arc<dyn RunExecutor>) -> anyhow::Result<()> {
        ensure!(
            Arc::ptr_eq(&self.executor, executor),
            "owned workspace requires the exact bound native VM executor; an executor replacement or wrapper is unsupported"
        );
        // Reauthenticate at admission, including after cloning the builder.
        self.base.workspace_owner()?;
        crate::environment_snapshot::SnapshotStore::new(&self.rootfs_store)?
            .open_prepared_rootfs(self._rootfs.reference())?;
        Ok(())
    }

    pub(crate) fn check_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> anyhow::Result<()> {
        for path in paths {
            let path = candidate(path)?;
            for store in [&self.workspace_store, &self.rootfs_store] {
                ensure!(
                    !overlap(&path, store),
                    "owned workspace path overlaps a private base store: {}",
                    path.display()
                );
            }
        }
        Ok(())
    }

    pub(crate) fn validate_overlay(
        &self,
        hint: &OverlayHint,
        storage: &Path,
    ) -> anyhow::Result<()> {
        ensure!(
            hint.execution_snapshot.is_none(),
            "owned workspace requires a fresh stage, not execution-snapshot backing"
        );
        ensure!(
            hint.lower_dirs.len() == 1 && hint.lower_dirs[0].canonicalize()? == self.target,
            "owned workspace requires exactly its live target as the single configured base; compose and replacement lowers are unsupported"
        );
        ensure!(
            hint.merged_dir.is_none(),
            "owned workspace requires a mountless native VM"
        );
        ensure!(
            !hint.protect_target,
            "owned workspace apply target must remain writable"
        );
        ensure!(
            hint.lower_mutability.len() <= 1,
            "owned workspace lower declarations cannot describe a composed stack"
        );
        let storage = candidate(storage)?;
        let stage = candidate(hint.stage_dir.as_deref().unwrap_or(&storage))?;
        self.check_paths(
            std::iter::once(storage.as_path())
                .chain(std::iter::once(stage.as_path()))
                .chain(hint.upper_dir.iter().map(PathBuf::as_path))
                .chain(hint.work_dir.iter().map(PathBuf::as_path)),
        )?;
        ensure!(
            !overlap(&stage, &self.target) && !overlap(&storage, &self.target),
            "owned workspace target, stage and storage must be independent trees"
        );
        Ok(())
    }

    pub(crate) fn validate_record(
        &self,
        record: &super::overlay::OverlayRecord,
        storage: &Path,
    ) -> anyhow::Result<()> {
        self.check_paths([
            storage,
            &record.stage_dir,
            &record.upper.upper_dir,
            &record.upper.work_dir,
            &record.stage_dir.join("preimages"),
            &record.merged_dir,
            &record.target,
        ])?;
        ensure!(
            candidate(&record.target)? == self.target,
            "owned workspace apply target changed during preparation"
        );
        let stage = candidate(&record.stage_dir)?;
        ensure!(
            !overlap(&stage, &self.target),
            "owned workspace stage overlaps its apply target"
        );
        // An existing sealed stage belongs to a different admission path. This
        // check precedes RunLease/Core initialization and never repairs it.
        for name in [
            "upper",
            "work",
            "preimages",
            "overlay.json",
            "run.json",
            RECEIPT,
        ] {
            ensure!(
                !exists(&stage.join(name))?,
                "owned workspace requires a fresh stage; existing {name} is unsupported"
            );
        }
        for path in [
            &record.upper.upper_dir,
            &record.upper.work_dir,
            &record.stage_dir.join("preimages"),
        ] {
            ensure!(
                !exists(path)?,
                "owned workspace backing already exists: {}",
                path.display()
            );
            ensure!(
                candidate(path)?.starts_with(&stage),
                "owned workspace backing must stay inside its fresh stage"
            );
            ensure!(
                !overlap(&candidate(path)?, &self.target),
                "owned workspace backing overlaps its apply target"
            );
        }
        Ok(())
    }

    pub(crate) fn write_receipt(&self, stage: &Path) -> anyhow::Result<()> {
        crate::util::write_private_json(
            &stage.join(RECEIPT),
            &WorkspaceBaseline {
                version: 1,
                workspace: crate::PreparedBase {
                    store: self.workspace_store.clone(),
                    base: self.base.reference().clone(),
                },
                rootfs: crate::PreparedBase {
                    store: self.rootfs_store.clone(),
                    base: self._rootfs.reference().clone(),
                },
                target: self.target.clone(),
            },
        )
    }

    pub(crate) fn check_handoff(
        &self,
        root_lowers: &[PathBuf],
        lowers: &[PathBuf],
        baseline: Option<&Path>,
        target: Option<&Path>,
        mutability: &[pvisor_overlay_core::LayerMutability],
    ) -> anyhow::Result<()> {
        ensure!(
            root_lowers
                .iter()
                .any(|path| path.canonicalize().ok().as_ref() == Some(&self._rootfs.root())),
            "owned workspace replaced the actual prepared rootfs device"
        );
        ensure!(
            lowers.len() == 1 && lowers[0].canonicalize()? == self.base.root(),
            "owned workspace native lower differs from its owner"
        );
        ensure!(
            baseline.map(Path::canonicalize).transpose()?.as_ref() == Some(&self.base.root())
                && target.map(Path::canonicalize).transpose()?.as_ref() == Some(&self.target)
                && mutability == [pvisor_overlay_core::LayerMutability::Immutable],
            "owned workspace native baseline/target/declarations differ from admission"
        );
        Ok(())
    }
}

fn exists(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub(crate) fn reject_stage_reconstruction(stage: &Path) -> anyhow::Result<()> {
    ensure!(
        !exists(&stage.join(RECEIPT))?,
        "CAPABILITY_UNSUPPORTED: owned workspace is fresh-only; stage reconstruction requires explicit ownership support"
    );
    Ok(())
}

pub(crate) fn reject_checkpoint_reconstruction(
    checkpoint: &crate::LogicalCheckpoint,
) -> anyhow::Result<()> {
    reject_stage_reconstruction(&checkpoint.source_stage)?;
    // Discovery is not evidence: only a checked seal plus successful public
    // workspace-catalog authentication establishes this conservative refusal.
    // An ordinary path, rootfs-only base or digest-looking name does not.
    for lower in &checkpoint.lower_dirs {
        if let Ok(Some(base)) = SnapshotBase::lease_root(lower)
            && base.workspace_owner().is_ok()
        {
            anyhow::bail!(
                "CAPABILITY_UNSUPPORTED: owned workspace is fresh-only; a catalog-backed checkpoint cannot reconstruct a new stage"
            );
        }
    }
    Ok(())
}

/// Retain a checked lease across the entire stopped-Job apply operation. A
/// normal composed or unauthenticated frozen lower is never admitted here.
pub(crate) fn apply_owner(
    record: &super::registry::RunRecord,
    overlay: &super::overlay::OverlayRecord,
) -> anyhow::Result<Option<ApplyWorkspaceOwner>> {
    if !exists(&overlay.stage_dir.join(RECEIPT))? {
        return Ok(None);
    }
    if record.overlay_lowers.len() != 1 {
        anyhow::bail!("owned workspace apply requires exactly one baseline lower");
    }
    let baseline = overlay
        .baseline_lower
        .as_ref()
        .context("owned workspace apply has no frozen baseline")?;
    let lower = record.overlay_lowers[0].canonicalize()?;
    ensure!(
        lower == baseline.canonicalize()?,
        "owned workspace lower differs from baseline"
    );
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(overlay.stage_dir.join(RECEIPT))?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 8192,
        "invalid workspace baseline receipt"
    );
    let receipt: WorkspaceBaseline = serde_json::from_reader(file)?;
    ensure!(
        receipt.version == 1 && receipt.target == overlay.target,
        "workspace baseline target or version mismatch"
    );
    let store = receipt.workspace.store.canonicalize()?;
    let rootfs_store = receipt.rootfs.store.canonicalize()?;
    ensure!(
        !overlap(&store, &rootfs_store),
        "workspace/rootfs catalogs overlap"
    );
    let base = crate::environment_snapshot::SnapshotStore::new(&store)?
        .open_prepared_workspace(&receipt.workspace.base)?;
    let rootfs = crate::environment_snapshot::SnapshotStore::new(&rootfs_store)?
        .open_prepared_rootfs(&receipt.rootfs.base)?;
    ensure!(
        base.root() == lower,
        "workspace apply baseline differs from its catalog generation"
    );
    for path in [
        &overlay.target,
        &overlay.stage_dir,
        &overlay.upper.upper_dir,
        &overlay.upper.work_dir,
        &record.storage,
    ] {
        ensure!(
            !overlap(&candidate(path)?, &store),
            "workspace apply path overlaps its base store"
        );
        ensure!(
            !overlap(&candidate(path)?, &rootfs_store),
            "workspace apply path overlaps its rootfs store"
        );
    }
    ensure!(
        !overlap(
            &candidate(&overlay.target)?,
            &candidate(&overlay.stage_dir)?
        ),
        "workspace apply target overlaps its stage"
    );
    Ok(Some(ApplyWorkspaceOwner {
        _workspace: base,
        _rootfs: rootfs,
    }))
}

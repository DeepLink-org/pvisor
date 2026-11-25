//! Project-scoped authority for completing a container retirement.
use super::*;
use pvisor_journal::api::{DurableFiles, Persistence};

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u8,
    project: String,
    service: String,
    id: String,
    device: u64,
    inode: u64,
}

impl Receipt {
    fn read(path: &Path) -> anyhow::Result<Option<Self>> {
        const LIMIT: u64 = 1024;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o777 == 0o600
                && metadata.nlink() == 1
                && metadata.len() <= LIMIT,
            "invalid Compose retirement receipt"
        );
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= LIMIT,
            "oversized Compose retirement receipt"
        );
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    fn matches_directory(&self, metadata: &fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && (metadata.dev(), metadata.ino()) == (self.device, self.inode)
    }
}

impl Store {
    fn compose_retirement_dir(&self, project: &str) -> anyhow::Result<PathBuf> {
        compose::Project::validate_name(project)?;
        Ok(self.root.join("compose").join(format!("{project}.retired")))
    }

    pub(super) fn prepare_compose_retirement(
        &self,
        instance: &Instance,
        member: &compose::Membership,
        directory: &Path,
    ) -> anyhow::Result<PathBuf> {
        ensure!(
            valid_container_id(&instance.id) && compose::service_name(&member.service),
            "invalid Compose retirement membership"
        );
        private_directory(directory)?;
        let metadata = directory.symlink_metadata()?;
        let receipts = self.compose_retirement_dir(&member.project)?;
        private_directory(&self.root.join("compose"))?;
        private_directory(&receipts)?;
        Persistence::sync_directory(&self.root)?;
        Persistence::sync_directory(&self.root.join("compose"))?;
        let path = receipts.join(&instance.id);
        let receipt = Receipt {
            schema: 1,
            project: member.project.clone(),
            service: member.service.clone(),
            id: instance.id.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        ensure!(
            Receipt::read(&path)
                .with_context(|| format!("invalid Compose retirement receipt: {}", path.display()))?
                .is_none_or(|existing| existing == receipt),
            "Compose retirement receipt does not match the active container: {}",
            path.display()
        );
        Persistence::atomic_write(&path, &serde_json::to_vec(&receipt)?, 0o600)?;
        Ok(path)
    }

    // The catalog lock covers receipt publication, rename and removal.
    pub(super) fn finish_compose_retirement(&self, receipt: &Path) -> anyhow::Result<()> {
        if private_directory_exists(&self.retired_root())? {
            Persistence::sync_directory(&self.retired_root())?;
        }
        Persistence::sync_directory(&self.root.join("instances"))?;
        fs::remove_file(receipt)?;
        Persistence::sync_directory(receipt.parent().unwrap())
    }

    pub(super) fn finish_project_retirements(&self, project: &str) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        let directory = self.compose_retirement_dir(project)?;
        if !private_directory_exists(&self.root.join("compose"))?
            || !private_directory_exists(&directory)?
        {
            return Ok(());
        }
        let entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
        for entry in entries {
            if let Some(id) = entry
                .file_name()
                .to_str()
                .filter(|id| valid_container_id(id))
            {
                self.retry_compose_retirement(project, id, None, &entry.path())?;
            }
        }
        Ok(())
    }

    // Promotion holds the catalog lock and already checked the candidate's
    // project, service and exact previous ID.
    pub(super) fn finish_replacement_retirement(
        &self,
        project: &str,
        service: &str,
        id: &str,
    ) -> anyhow::Result<()> {
        ensure!(
            valid_container_id(id),
            "invalid Compose replacement source ID"
        );
        let directory = self.compose_retirement_dir(project)?;
        if !private_directory_exists(&self.root.join("compose"))?
            || !private_directory_exists(&directory)?
        {
            return Ok(());
        }
        self.retry_compose_retirement(project, id, Some(service), &directory.join(id))
    }

    fn retry_compose_retirement(
        &self,
        project: &str,
        id: &str,
        service: Option<&str>,
        path: &Path,
    ) -> anyhow::Result<()> {
        let context = || {
            format!(
                "cannot reconcile Compose retirement receipt: {}",
                path.display()
            )
        };
        let Some(receipt) = Receipt::read(path).with_context(context)? else {
            // Old retirements have no project authority; only exact-ID rm can reap them.
            return Ok(());
        };
        ensure!(
            receipt.schema == 1
                && receipt.project == project
                && receipt.id == id
                && compose::service_name(&receipt.service)
                && service.is_none_or(|service| receipt.service == service),
            "{}: ownership mismatch",
            context()
        );
        let retired = self.retired_directory(id);
        let retired_metadata = if private_directory_exists(&self.retired_root())? {
            metadata_if_exists(&retired)?
        } else {
            None
        };
        if let Some(active) = metadata_if_exists(&self.directory(id))? {
            // Receipt publication alone does not authorize removing an active directory.
            ensure!(
                retired_metadata.is_none() && receipt.matches_directory(&active),
                "{}: active directory conflicts with retirement",
                context()
            );
            return Ok(());
        }
        if let Some(metadata) = retired_metadata {
            ensure!(
                receipt.matches_directory(&metadata),
                "{}: retired directory identity changed",
                context()
            );
            self.reap_retired(&retired, id)?;
        }
        // An absent directory also covers a crash or exact-ID rm after deletion.
        self.finish_compose_retirement(path)
    }
}

fn metadata_if_exists(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match path.symlink_metadata() {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn private_directory_exists(path: &Path) -> anyhow::Result<bool> {
    if metadata_if_exists(path)?.is_none() {
        return Ok(false);
    }
    private_directory(path)?;
    Ok(true)
}

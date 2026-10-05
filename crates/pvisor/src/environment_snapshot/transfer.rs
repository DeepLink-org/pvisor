//! Immutable, host-owned checkpoint distribution. Import preserves the original
//! snapshot identity and compatibility binding; it does not authorize migration.
use super::{
    Compatibility, EnvironmentManifest, PublishedEnvironment, SnapshotStore, TreeEntry,
    TreeInventory, TreeObject, file_hash, native_path, store,
};
use crate::image::cache::storage::{S3, Storage, is_conflict, validate_key};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const NAMESPACE: &str = "pvisor-checkpoints-v1";
const CHUNK: u64 = 1024 * 1024;
const MAX_METADATA: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 65_536;
const MAX_CHUNKS: u64 = 65_536;
const MAX_BYTES: u64 = MAX_CHUNKS * CHUNK;

pub use pvisor_core::operation::SnapshotTransfer;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    bytes: u64,
    sha256: String,
    // None represents exactly one zero-filled chunk, including a short tail.
    chunks: Vec<Option<String>>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Ram {
    Raw { payload: Payload },
    Compressed { objects: BTreeMap<String, Payload> },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferManifest {
    version: u32,
    snapshot_id: String,
    machine: Payload,
    ram: Ram,
    // Canonical primary files in the original inventory's order. No imported
    // path or metadata can be substituted by the transport manifest.
    files: Vec<Payload>,
}

/// Synchronous bounded-chunk I/O, sharing the native cache's process-wide S3
/// runtime. The caller owns cancellation/lease renewal around a bulk transfer.
pub struct SnapshotRepository {
    storage: Storage,
    read_only: bool,
}
impl SnapshotRepository {
    pub fn filesystem(root: &Path, read_only: bool) -> anyhow::Result<Self> {
        ensure!(
            root.is_absolute(),
            "checkpoint repository path must be absolute"
        );
        if !read_only {
            crate::util::create_dir_all_durable(root)?;
            File::open(root.parent().unwrap_or(root))?.sync_all()?;
            let root = root.canonicalize()?;
            // Establish and sync the fixed namespace once. Per-object writes
            // then need only the backend's atomic file/parent-directory sync.
            for relative in [
                NAMESPACE.to_owned(),
                format!("{NAMESPACE}/chunks"),
                format!("{NAMESPACE}/manifests"),
                format!("{NAMESPACE}/transfers"),
            ] {
                let path = root.join(relative);
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                ensure!(
                    fs::symlink_metadata(&path)?.is_dir(),
                    "invalid checkpoint repository directory"
                );
                File::open(&path)?.sync_all()?;
                File::open(
                    path.parent()
                        .context("missing checkpoint repository parent")?,
                )?
                .sync_all()?;
            }
        }
        Ok(Self {
            storage: Storage::filesystem(root.to_owned(), false)?,
            read_only,
        })
    }
    pub fn s3(location: &str, read_only: bool) -> anyhow::Result<Self> {
        Ok(Self {
            storage: Storage::s3(location)?,
            read_only,
        })
    }
    /// Use a host-provided client for explicit workload credentials/endpoints.
    pub fn object_store(
        client: Arc<dyn object_store::ObjectStore>,
        prefix: &str,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        if !prefix.is_empty() {
            validate_key(prefix)?;
        }
        Ok(Self {
            storage: Storage::S3(Arc::new(S3::new(client, prefix.into())?)),
            read_only,
        })
    }
    fn get(&self, class: &str, id: &str, maximum: usize) -> anyhow::Result<Vec<u8>> {
        store::valid_id(id)?;
        let bytes = self
            .storage
            .get_bounded(&format!("{NAMESPACE}/{class}/{id}"), maximum)?
            .context("missing checkpoint transfer object")?;
        ensure!(
            bytes.len() <= maximum && store::digest(&bytes) == id,
            "checkpoint transfer object size or digest mismatch"
        );
        Ok(bytes)
    }
    fn put(&self, class: &str, bytes: Vec<u8>) -> anyhow::Result<String> {
        ensure!(!self.read_only, "checkpoint repository is read-only");
        let id = store::digest(&bytes);
        let key = format!("{NAMESPACE}/{class}/{id}");
        match self.storage.compare_and_swap(&key, bytes.clone(), None) {
            Ok(()) => {}
            Err(error) if is_conflict(&error) => {
                ensure!(
                    self.storage.get_bounded(&key, bytes.len())?.as_deref()
                        == Some(bytes.as_slice()),
                    "checkpoint content collision or corruption"
                );
            }
            Err(error) => return Err(error),
        }
        Ok(id)
    }
    fn send_file(
        &self,
        path: &Path,
        seen: &mut BTreeSet<String>,
        budget: &mut Budget,
    ) -> anyhow::Result<Payload> {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let before = input.metadata()?;
        ensure!(
            before.is_file() && before.len() <= MAX_BYTES,
            "invalid checkpoint payload"
        );
        let payload = self.send_reader(&mut input, before.len(), seen, budget)?;
        let after = input.metadata()?;
        ensure!(
            before.len() == after.len()
                && before.ctime() == after.ctime()
                && before.ctime_nsec() == after.ctime_nsec(),
            "checkpoint payload changed during transfer"
        );
        Ok(payload)
    }
    fn send_reader(
        &self,
        mut input: impl Read,
        length: u64,
        seen: &mut BTreeSet<String>,
        budget: &mut Budget,
    ) -> anyhow::Result<Payload> {
        ensure!(length <= MAX_BYTES, "checkpoint payload exceeds byte limit");
        budget.charge(length, length.div_ceil(CHUNK))?;
        let mut digest = Sha256::new();
        let mut chunks = Vec::new();
        let mut remaining = length;
        while remaining > 0 {
            let mut bytes = vec![0; remaining.min(CHUNK) as usize];
            input.read_exact(&mut bytes)?;
            remaining -= bytes.len() as u64;
            digest.update(&bytes);
            if bytes.iter().all(|byte| *byte == 0) {
                chunks.push(None);
            } else {
                let id = store::digest(&bytes);
                if seen.insert(id.clone()) {
                    self.put("chunks", bytes)?;
                }
                chunks.push(Some(id));
            }
        }
        Ok(Payload {
            bytes: length,
            sha256: crate::util::encode_hex(&digest.finalize()),
            chunks,
        })
    }
    /// Commit the immutable transfer manifest last. Interrupted exports expose
    /// no receipt; repeated exports reuse content and yield the same receipt.
    pub fn publish(&self, source: &PublishedEnvironment) -> anyhow::Result<SnapshotTransfer> {
        ensure!(!self.read_only, "checkpoint repository is read-only");
        let directory = source.directory();
        let bytes = fs::read(directory.join("manifest.json"))?;
        ensure!(
            bytes.len() <= MAX_METADATA,
            "checkpoint manifest exceeds transfer limit"
        );
        let snapshot_id = store::digest(&bytes);
        let manifest: EnvironmentManifest = serde_json::from_slice(&bytes)?;
        validate_manifest(&manifest)?;
        ensure!(
            serde_json::to_vec(&manifest)? == serde_json::to_vec(source.manifest())?,
            "checkpoint manifest changed during transfer"
        );
        let mut seen = BTreeSet::new();
        let mut budget = Budget::default();
        let machine = self.send_file(&directory.join("machine.json"), &mut seen, &mut budget)?;
        let ram = if let Some(blocks) = &manifest.ram_blocks {
            let mut objects = BTreeMap::new();
            for block in &blocks.blocks {
                if !objects.contains_key(&block.id) {
                    objects.insert(
                        block.id.clone(),
                        self.send_file(
                            &directory.join("ram-blocks").join(&block.id),
                            &mut seen,
                            &mut budget,
                        )?,
                    );
                }
            }
            Ram::Compressed { objects }
        } else {
            Ram::Raw {
                payload: self.send_file(&directory.join("ram.bin"), &mut seen, &mut budget)?,
            }
        };
        let mut files = Vec::new();
        let complete = source.complete_inventory()?;
        for entry in primary_files(&complete) {
            let private = manifest
                .filesystem_blocks
                .as_ref()
                .filter(|blocks| blocks.file(&entry.path).is_some());
            let payload = if let Some(blocks) = private {
                let TreeObject::File { bytes, .. } = &entry.object else {
                    unreachable!()
                };
                self.send_reader(
                    blocks.reader(&directory.join("filesystem-blocks"), &entry.path, *bytes)?,
                    *bytes,
                    &mut seen,
                    &mut budget,
                )?
            } else {
                self.send_file(&source.file_path(&entry.path)?, &mut seen, &mut budget)?
            };
            files.push(payload);
        }
        let transfer = TransferManifest {
            version: 1,
            snapshot_id: snapshot_id.clone(),
            machine,
            ram,
            files,
        };
        validate_transfer(&transfer, &manifest)?;
        // Verify the complete source once more before exposing an importable
        // receipt, including data not visited by the original VM.
        source.verify_filesystems()?;
        ensure!(
            file_hash(&directory.join("machine.json"))? == manifest.machine_sha256,
            "checkpoint machine changed"
        );
        if let Some(blocks) = &manifest.ram_blocks {
            blocks.decode(
                &directory.join("ram-blocks"),
                std::io::sink(),
                &manifest.ram_sha256,
            )?;
        } else {
            ensure!(
                file_hash(&directory.join("ram.bin"))? == manifest.ram_sha256,
                "checkpoint RAM changed"
            );
        }
        ensure!(
            self.put("manifests", bytes)? == snapshot_id,
            "checkpoint manifest changed"
        );
        let bytes = serde_json::to_vec(&transfer)?;
        ensure!(
            bytes.len() <= MAX_METADATA,
            "checkpoint transfer manifest exceeds limit"
        );
        Ok(SnapshotTransfer {
            version: 1,
            snapshot_id,
            transfer_id: self.put("transfers", bytes)?,
        })
    }
    fn receive_file(&self, payload: &Payload, destination: &Path) -> anyhow::Result<()> {
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        output.set_len(payload.bytes)?;
        let mut offset = 0;
        let mut digest = Sha256::new();
        for id in &payload.chunks {
            let length = (payload.bytes - offset).min(CHUNK);
            if let Some(id) = id {
                let bytes = self.get("chunks", id, CHUNK as usize)?;
                ensure!(
                    bytes.len() as u64 == length,
                    "checkpoint chunk length mismatch"
                );
                digest.update(&bytes);
                output.write_all_at(&bytes, offset)?;
            } else {
                digest.update(vec![0; length as usize]);
            }
            offset += length;
        }
        ensure!(
            crate::util::encode_hex(&digest.finalize()) == payload.sha256,
            "checkpoint payload digest mismatch"
        );
        output.sync_all()?;
        Ok(())
    }
    /// Fetch only through the configured repository, verify the original seal
    /// and every byte, then atomically publish under the original snapshot id.
    /// A mismatching host compatibility binding is rejected before bulk reads.
    pub fn import(
        &self,
        destination: &SnapshotStore,
        receipt: &SnapshotTransfer,
        expected: &Compatibility,
    ) -> anyhow::Result<()> {
        receipt.validate()?;
        let transfer: TransferManifest =
            serde_json::from_slice(&self.get("transfers", &receipt.transfer_id, MAX_METADATA)?)?;
        ensure!(
            transfer.version == 1 && transfer.snapshot_id == receipt.snapshot_id,
            "checkpoint transfer binding mismatch"
        );
        let bytes = self.get("manifests", &receipt.snapshot_id, MAX_METADATA)?;
        let manifest: EnvironmentManifest = serde_json::from_slice(&bytes)?;
        ensure!(
            manifest.compatibility == *expected,
            "checkpoint transfer compatibility mismatch"
        );
        validate_manifest(&manifest)?;
        validate_transfer(&transfer, &manifest)?;
        let pending = destination.begin()?;
        let directory = pending.directory();
        let mut header = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("manifest.json"))?;
        header.write_all(&bytes)?;
        header.sync_all()?;
        self.receive_file(&transfer.machine, &directory.join("machine.json"))?;
        match &transfer.ram {
            Ram::Raw { payload } => self.receive_file(payload, &directory.join("ram.bin"))?,
            Ram::Compressed { objects } => {
                fs::create_dir(directory.join("ram-blocks"))?;
                for (id, payload) in objects {
                    self.receive_file(payload, &directory.join("ram-blocks").join(id))?;
                }
                File::open(directory.join("ram-blocks"))?.sync_all()?;
            }
        }
        if let Some(blocks) = &manifest.ram_blocks {
            pending.share_imported_ram(blocks)?;
        } else if let Some(index) = &manifest.ram_index {
            let actual = super::RawRamIndex::capture(&File::open(directory.join("ram.bin"))?)?;
            ensure!(
                actual.length == index.length && actual.sha256 == index.sha256,
                "checkpoint raw RAM index mismatch"
            );
        }
        let root = directory.join("rootfs");
        fs::create_dir(&root)?;
        let _cleanup = ImportCleanup(root.clone());
        let mut files = transfer.files.iter();
        let complete =
            super::layers::complete_inventory(&manifest.filesystem, &manifest.filesystem_layers)?;
        for entry in &complete.entries {
            let path = root.join(OsStr::from_bytes(&entry.path));
            match &entry.object {
                TreeObject::Directory => {
                    if !entry.path.is_empty() {
                        fs::create_dir(&path)?;
                    }
                }
                TreeObject::File { hardlink, .. } | TreeObject::Symlink { hardlink, .. }
                    if *hardlink != entry.path =>
                {
                    fs::hard_link(root.join(OsStr::from_bytes(hardlink)), &path)?;
                }
                TreeObject::File { .. } => {
                    self.receive_file(files.next().context("missing filesystem payload")?, &path)?
                }
                TreeObject::Symlink { target, .. } => symlink(OsStr::from_bytes(target), &path)?,
            }
        }
        // Set directory timestamps/ACLs last, after all children exist. Metadata
        // is applied without following symlinks; only an owned pending tree changes.
        for entry in complete.entries.iter().rev() {
            let path = root.join(OsStr::from_bytes(&entry.path));
            restore_metadata(&path, entry)?;
            if !matches!(entry.object, TreeObject::Symlink { .. }) {
                File::open(&path)?.sync_all()?;
            }
        }
        super::verify_tree(&root, &complete)?;
        pending.import_filesystem_layers(&manifest.filesystem_layers)?;
        // Removing adopted lower directories changed the private root's mtime.
        // Restore only that owned root after all layer references are durable.
        if !manifest.filesystem_layers.is_empty() {
            restore_metadata(&root, &manifest.filesystem.entries[0])?;
            File::open(&root)?.sync_all()?;
        }
        pending.import_filesystem_blocks(&manifest)?;
        pending.commit_import(&receipt.snapshot_id, expected)
    }
}

// A valid read-only directory, or failed hostile metadata, may otherwise leave
// an unremovable pending tree. This guard drops before PendingEnvironment. A
// successful rename makes this old private path disappear; duplicate imports
// and failures restore traversal/write permissions only on their staging tree.
struct ImportCleanup(PathBuf);
impl Drop for ImportCleanup {
    fn drop(&mut self) {
        fn prepare(path: &Path) -> std::io::Result<()> {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                for child in fs::read_dir(path)? {
                    prepare(&child?.path())?;
                }
            }
            Ok(())
        }
        let _ = prepare(&self.0);
    }
}

fn primary_files(tree: &TreeInventory) -> impl Iterator<Item = &TreeEntry> {
    tree.entries.iter().filter(|entry| matches!(&entry.object, TreeObject::File { hardlink, .. } if *hardlink == entry.path))
}
fn relative(bytes: &[u8]) -> anyhow::Result<PathBuf> {
    ensure!(
        bytes.len() <= 4096 && !bytes.contains(&0),
        "invalid checkpoint tree path"
    );
    let path = PathBuf::from(OsStr::from_bytes(bytes));
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_)))
            && path.as_os_str().as_bytes() == bytes
            && !bytes
                .split(|byte| *byte == b'/')
                .any(|part| part == b"." || part == b".." || part.is_empty()),
        "checkpoint tree path must be canonical and relative"
    );
    Ok(path)
}
fn validate_manifest(manifest: &EnvironmentManifest) -> anyhow::Result<()> {
    store::valid_id(&manifest.ram_sha256)?;
    store::valid_id(&manifest.machine_sha256)?;
    ensure!(
        matches!(
            (manifest.version, &manifest.ram_blocks, &manifest.ram_index),
            (1, None, None)
                | (2, Some(_), None)
                | (3, None, Some(_))
                | (4, Some(_), None)
                | (4, None, Some(_))
                | (5, Some(_), None)
                | (5, None, Some(_))
        ) && manifest.filesystem.version == 1,
        "invalid checkpoint inventory/version"
    );
    manifest.validate_filesystem_format()?;
    if let Some(blocks) = &manifest.ram_blocks {
        blocks.validate()?;
    }
    if let Some(index) = &manifest.ram_index {
        index.validate()?;
    }
    let complete =
        super::layers::complete_inventory(&manifest.filesystem, &manifest.filesystem_layers)?;
    ensure!(
        !complete.entries.is_empty() && complete.entries.len() <= MAX_ENTRIES,
        "checkpoint tree exceeds inventory limit"
    );
    for layer in &manifest.filesystem_layers {
        super::filesystems::validate_identity(layer)?;
        validate_tree_metadata(&layer.filesystem)?;
    }
    validate_tree_metadata(&manifest.filesystem)?;
    validate_tree_metadata(&complete)
}

pub(super) fn validate_tree_metadata(tree: &TreeInventory) -> anyhow::Result<()> {
    ensure!(
        tree.version == 1 && !tree.entries.is_empty(),
        "invalid checkpoint tree inventory"
    );
    let mut previous = BTreeMap::<PathBuf, &TreeEntry>::new();
    for (index, entry) in tree.entries.iter().enumerate() {
        let path = if index == 0 {
            ensure!(
                entry.path.is_empty() && matches!(entry.object, TreeObject::Directory),
                "missing checkpoint tree root"
            );
            PathBuf::new()
        } else {
            let path = relative(&entry.path)?;
            ensure!(
                previous
                    .get(path.parent().context("missing checkpoint parent")?)
                    .is_some_and(|entry| matches!(entry.object, TreeObject::Directory)),
                "checkpoint parent must be an earlier directory"
            );
            path
        };
        ensure!(
            entry.acl.is_none() && (0..1_000_000_000).contains(&entry.mtime_nsec),
            "invalid Linux checkpoint metadata"
        );
        let kind = entry.mode & libc::S_IFMT;
        match &entry.object {
            TreeObject::Directory => {
                ensure!(kind == libc::S_IFDIR, "checkpoint directory type mismatch")
            }
            TreeObject::File {
                bytes,
                sha256,
                hardlink,
            } => {
                ensure!(
                    kind == libc::S_IFREG && *bytes <= MAX_BYTES,
                    "checkpoint file type/size mismatch"
                );
                store::valid_id(sha256)?;
                validate_link(entry, hardlink, &previous)?;
            }
            TreeObject::Symlink { target, hardlink } => {
                ensure!(
                    kind == libc::S_IFLNK
                        && !target.is_empty()
                        && target.len() <= 4096
                        && !target.contains(&0),
                    "invalid checkpoint symlink"
                );
                validate_link(entry, hardlink, &previous)?;
            }
        }
        ensure!(
            previous.insert(path, entry).is_none(),
            "duplicate checkpoint tree path"
        );
    }
    Ok(())
}
fn validate_link(
    entry: &TreeEntry,
    origin: &[u8],
    previous: &BTreeMap<PathBuf, &TreeEntry>,
) -> anyhow::Result<()> {
    let path = relative(origin)?;
    if origin != entry.path {
        let mut canonical = (*previous
            .get(&path)
            .context("missing earlier checkpoint hardlink origin")?)
        .clone();
        canonical.path = entry.path.clone();
        ensure!(canonical == *entry, "checkpoint hardlink metadata mismatch");
    }
    Ok(())
}
#[derive(Default)]
struct Budget {
    bytes: u64,
    chunks: u64,
}
impl Budget {
    fn charge(&mut self, bytes: u64, chunks: u64) -> anyhow::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("checkpoint payload size overflow")?;
        self.chunks = self
            .chunks
            .checked_add(chunks)
            .context("checkpoint chunk count overflow")?;
        ensure!(
            self.bytes <= MAX_BYTES && self.chunks <= MAX_CHUNKS,
            "checkpoint exceeds transfer budget"
        );
        Ok(())
    }
}
fn validate_transfer(
    transfer: &TransferManifest,
    manifest: &EnvironmentManifest,
) -> anyhow::Result<()> {
    let mut budget = Budget::default();
    let mut payload = |value: &Payload| -> anyhow::Result<()> {
        ensure!(
            value.chunks.len() as u64 == value.bytes.div_ceil(CHUNK),
            "invalid checkpoint payload geometry"
        );
        budget.charge(value.bytes, value.chunks.len() as u64)?;
        store::valid_id(&value.sha256)?;
        for id in value.chunks.iter().flatten() {
            store::valid_id(id)?;
        }
        Ok(())
    };
    ensure!(
        transfer.machine.bytes > 0 && transfer.machine.bytes <= 128 * 1024 * 1024,
        "invalid checkpoint machine payload size"
    );
    ensure!(
        transfer.machine.sha256 == manifest.machine_sha256,
        "checkpoint machine payload binding mismatch"
    );
    payload(&transfer.machine)?;
    match (&transfer.ram, &manifest.ram_blocks) {
        (Ram::Raw { payload: ram }, None) => {
            ensure!(
                ram.bytes > 0
                    && manifest
                        .ram_index
                        .as_ref()
                        .is_none_or(|index| index.length == ram.bytes),
                "checkpoint RAM length mismatch"
            );
            ensure!(
                ram.sha256 == manifest.ram_sha256,
                "checkpoint RAM payload binding mismatch"
            );
            payload(ram)?;
        }
        (Ram::Compressed { objects }, Some(blocks)) => {
            let ids = blocks
                .blocks
                .iter()
                .map(|block| &block.id)
                .collect::<BTreeSet<_>>();
            ensure!(
                objects.keys().collect::<BTreeSet<_>>() == ids,
                "checkpoint RAM object inventory mismatch"
            );
            for value in objects.values() {
                ensure!(
                    (46..=45 + crate::ram_backing::BLOCK_BYTES as u64).contains(&value.bytes),
                    "invalid compressed RAM frame size"
                );
                payload(value)?;
            }
            ensure!(
                blocks.length <= MAX_BYTES,
                "checkpoint decoded RAM exceeds budget"
            );
        }
        _ => anyhow::bail!("checkpoint RAM encoding mismatch"),
    }
    let complete =
        super::layers::complete_inventory(&manifest.filesystem, &manifest.filesystem_layers)?;
    let files = primary_files(&complete).collect::<Vec<_>>();
    ensure!(
        files.len() == transfer.files.len(),
        "checkpoint filesystem payload inventory mismatch"
    );
    for (entry, value) in files.into_iter().zip(&transfer.files) {
        let TreeObject::File { bytes, sha256, .. } = &entry.object else {
            unreachable!()
        };
        ensure!(
            *bytes == value.bytes,
            "checkpoint filesystem payload length mismatch"
        );
        ensure!(
            *sha256 == value.sha256,
            "checkpoint file payload binding mismatch"
        );
        payload(value)?;
    }
    Ok(())
}
/// Apply a verified entry only to a caller-owned, unpublished staging object.
pub(super) fn restore_metadata(path: &Path, entry: &TreeEntry) -> anyhow::Result<()> {
    let native = native_path(path)?;
    let current = fs::symlink_metadata(path)?;
    if current.uid() != entry.uid || current.gid() != entry.gid {
        ensure!(
            unsafe { libc::lchown(native.as_ptr(), entry.uid, entry.gid) } == 0,
            "restore checkpoint ownership: {}",
            std::io::Error::last_os_error()
        );
    }
    if !matches!(entry.object, TreeObject::Symlink { .. }) {
        // A preceding alias may already have restored this private inode's
        // read-only mode. Xattrs still need write permission; all hard links
        // are closed within staging, and final mode is restored below.
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if current.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    for (name, _) in super::linux::xattrs(path)? {
        if !entry.xattrs.iter().any(|(expected, _)| *expected == name) {
            let name = CString::new(name)?;
            ensure!(
                unsafe { libc::lremovexattr(native.as_ptr(), name.as_ptr()) } == 0,
                "remove inherited checkpoint xattr: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    // Access ACL restoration may tighten mode before chmod; apply it after
    // every other xattr, including when visiting a read-only hard-link alias.
    for (name, value) in entry
        .xattrs
        .iter()
        .filter(|(name, _)| name != b"system.posix_acl_access")
        .chain(
            entry
                .xattrs
                .iter()
                .filter(|(name, _)| name == b"system.posix_acl_access"),
        )
    {
        let name = CString::new(name.as_slice())?;
        ensure!(
            unsafe {
                libc::lsetxattr(
                    native.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } == 0,
            "restore checkpoint xattr: {}",
            std::io::Error::last_os_error()
        );
    }
    if !matches!(entry.object, TreeObject::Symlink { .. }) {
        fs::set_permissions(path, fs::Permissions::from_mode(entry.mode & 0o7777))?;
    }
    let times = [libc::timespec {
        tv_sec: entry.mtime,
        tv_nsec: entry.mtime_nsec,
    }; 2];
    ensure!(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                native.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "restore checkpoint timestamps: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

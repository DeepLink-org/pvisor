//! Seal terminal native evidence before upload. Network retries and Worker
//! restarts read only this immutable spool, never a changing guest workspace.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Component,
};
use tokio::io::AsyncReadExt;

fn file(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= ARTIFACT_FILE_BYTES,
        "artifact source is not a bounded regular file"
    );
    Ok(file)
}
fn read(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let input = file(path)?;
    ensure!(
        input.metadata()?.len() <= limit,
        "artifact record exceeds limit"
    );
    let mut bytes = vec![];
    input.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "artifact record grew beyond limit"
    );
    Ok(bytes)
}
fn bundle_matches(
    path: &Path,
    result: &pvisor_core::RunResult,
) -> anyhow::Result<pvisor::RunBundle> {
    let bundle: pvisor::RunBundle = serde_json::from_slice(&read(path, ARTIFACT_FILE_BYTES)?)?;
    ensure!(
        bundle.schema_version == pvisor::RUN_BUNDLE_SCHEMA_VERSION
            && bundle.run.run_id == result.run_id.as_str()
            && bundle.run.attempt_id == result.attempt_id.as_str()
            && bundle.run.state == result.state
            && bundle.run.started_at_unix_ms == result.started_at_unix_ms
            && bundle.run.finished_at_unix_ms == result.finished_at_unix_ms
            && bundle.run.exit_code == result.exit_code,
        "native Run Bundle does not match completed attempt"
    );
    Ok(bundle)
}
fn create(path: &Path) -> anyhow::Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}
fn seal(file: File) -> anyhow::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o400))?;
    file.sync_all()?;
    Ok(())
}
fn copy(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let mut source = file(source)?;
    let size = source.metadata()?.len();
    let mut output = create(destination)?;
    ensure!(
        std::io::copy(
            &mut Read::by_ref(&mut source).take(ARTIFACT_FILE_BYTES + 1),
            &mut output
        )? == size,
        "artifact changed during copy"
    );
    seal(output)
}

struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other(
                "workspace archive exceeds artifact limit",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.remaining -= count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn archive_upper(
    storage: &Path,
    bundle: &pvisor::RunBundle,
    destination: &Path,
) -> anyhow::Result<()> {
    ensure!(
        bundle
            .executor_plan
            .as_ref()
            .is_some_and(|p| p.isolation == pvisor_core::IsolationKind::VirtualMachine),
        "workspace archive requires native VM evidence"
    );
    let upper = &bundle
        .filesystem
        .as_ref()
        .context("native VM has no private upper")?
        .upper;
    let relative = upper
        .strip_prefix(storage)
        .context("native upper is outside this Attempt's owned storage")?;
    ensure!(
        !relative.as_os_str().is_empty(),
        "cannot archive the entire Attempt storage"
    );
    let mut path = storage.to_owned();
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "invalid owned upper path"
        );
        path.push(component);
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "owned upper path is not a directory"
        );
    }
    archive_tree(upper, destination)
}

fn archive_suspended(
    storage: &Path,
    result: &pvisor_core::RunResult,
    bundle: &pvisor::RunBundle,
    destination: &Path,
    filesystem_pool: Option<&Path>,
) -> anyhow::Result<()> {
    use pvisor::environment_snapshot::{EnvironmentManifest, SnapshotStore};
    use std::os::unix::ffi::OsStrExt;
    let checkpoint = pvisor_core::operation::ExecutionSuspension::from_result(result)?.checkpoint;
    ensure!(
        checkpoint.store == storage.join("execution-snapshots")
            && checkpoint.source_run_id == result.run_id.as_str()
            && checkpoint.source_attempt_id == result.attempt_id.as_str(),
        "suspended workspace is outside this Attempt"
    );
    let manifest: EnvironmentManifest = serde_json::from_slice(&read(
        &checkpoint
            .store
            .join("objects")
            .join(&checkpoint.snapshot_id)
            .join("manifest.json"),
        ARTIFACT_FILE_BYTES,
    )?)?;
    let store = match filesystem_pool {
        Some(pool) => SnapshotStore::with_filesystem_pool(&checkpoint.store, pool)?,
        None => SnapshotStore::new(&checkpoint.store)?,
    };
    let published = store.open_for_restore(&checkpoint.snapshot_id, &manifest.compatibility)?;
    let machine: serde_json::Value = serde_json::from_slice(&published.machine_bytes()?)?;
    ensure!(
        machine["run_id"] == checkpoint.source_run_id
            && machine["attempt_id"] == checkpoint.source_attempt_id,
        "suspended workspace identity mismatch"
    );
    let root_overlay = bundle
        .filesystem
        .as_ref()
        .context("missing suspended filesystem")?
        .root_overlay;
    let device = if !root_overlay && machine["workspace"].is_object() {
        &machine["workspace"]
    } else {
        &machine["root"]
    };
    let upper = Path::new(device["upper"].as_str().context("missing captured upper")?);
    let source = Path::new(std::ffi::OsStr::from_bytes(
        &published.manifest().source_root,
    ));
    let direct = machine["private_sources"].as_array().and_then(|sources| {
        sources
            .iter()
            .find(|captured| captured["source"].as_str() == upper.to_str())
    });
    let relative = if machine["version"] == 2 {
        ensure!(
            published.manifest().version == 5,
            "direct private upper requires a packed snapshot"
        );
        Path::new(
            direct.context("direct private upper has no sealed source binding")?["path"]
                .as_str()
                .context("invalid direct private upper path")?,
        )
    } else {
        upper
            .strip_prefix(source)
            .context("captured upper escapes snapshot")?
    };
    archive_snapshot_layer(&published, relative, destination)
}

fn archive_snapshot_layer(
    published: &pvisor::environment_snapshot::PublishedEnvironment,
    relative: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    if published.manifest().filesystem_blocks.is_some() {
        use pvisor::environment_snapshot::TreeObject;
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            relative.components().count() == 1
                && relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "snapshot upper must be one private root"
        );
        let prefix = relative.as_os_str().as_bytes();
        let mut tar = tar::Builder::new(LimitedWriter {
            inner: create(destination)?,
            remaining: ARTIFACT_FILE_BYTES,
        });
        let mut found = false;
        for entry in &published.manifest().filesystem.entries {
            let tail = if entry.path == prefix {
                ensure!(
                    entry.object == TreeObject::Directory,
                    "snapshot upper is not a directory"
                );
                found = true;
                b"".as_slice()
            } else {
                let Some(tail) = entry
                    .path
                    .strip_prefix(prefix)
                    .and_then(|tail| tail.strip_prefix(b"/"))
                else {
                    continue;
                };
                tail
            };
            let name = Path::new("upper").join(std::ffi::OsStr::from_bytes(tail));
            let mut header = tar::Header::new_gnu();
            header.set_mode(entry.mode & 0o7777);
            header.set_uid(entry.uid.into());
            header.set_gid(entry.gid.into());
            header.set_mtime(0);
            match &entry.object {
                TreeObject::Directory => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_cksum();
                    tar.append_data(&mut header, &name, std::io::empty())?;
                }
                TreeObject::Symlink { target, .. } => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_cksum();
                    tar.append_link(&mut header, &name, std::ffi::OsStr::from_bytes(target))?;
                }
                TreeObject::File { bytes, .. } => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(*bytes);
                    header.set_cksum();
                    tar.append_data(
                        &mut header,
                        &name,
                        published.open_file(Path::new(std::ffi::OsStr::from_bytes(&entry.path)))?,
                    )?;
                }
            }
        }
        ensure!(found, "missing private snapshot upper");
        return seal(tar.into_inner()?.inner);
    }
    archive_tree(&published.owned_layer_path(relative)?, destination)
}

fn archive_tree(upper: &Path, destination: &Path) -> anyhow::Result<()> {
    let output = create(destination)?;
    let mut tar = tar::Builder::new(LimitedWriter {
        inner: output,
        remaining: ARTIFACT_FILE_BYTES,
    });
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    let mut pending = vec![PathBuf::new()];
    let mut count = 0;
    while let Some(relative) = pending.pop() {
        count += 1;
        ensure!(count <= 100_000, "workspace archive has too many entries");
        let path = upper.join(&relative);
        let metadata = fs::symlink_metadata(&path)?;
        let name = Path::new("upper").join(&relative);
        if metadata.is_dir() {
            tar.append_dir(&name, &path)?;
            let mut children = fs::read_dir(&path)?
                .map(|entry| entry.map(|entry| relative.join(entry.file_name())))
                .collect::<Result<Vec<_>, _>>()?;
            children.sort();
            pending.extend(children.into_iter().rev());
        } else if metadata.is_symlink() {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(metadata.permissions().mode() & 0o777);
            header.set_cksum();
            tar.append_link(&mut header, &name, fs::read_link(&path)?)?;
        } else {
            ensure!(
                metadata.is_file(),
                "workspace contains unsupported special file"
            );
            let mut input = file(&path)?;
            let opened = input.metadata()?;
            ensure!(
                opened.dev() == metadata.dev()
                    && opened.ino() == metadata.ino()
                    && opened.len() == metadata.len(),
                "workspace entry changed during archive"
            );
            tar.append_file(&name, &mut input)?;
        }
    }
    seal(tar.into_inner()?.inner)
}

fn describe(directory: &Path, name: &str) -> anyhow::Result<ArtifactFile> {
    let mut input = file(&directory.join(name))?;
    let mut chunks = vec![];
    let mut bytes = 0;
    let mut whole = blake3::Hasher::new();
    loop {
        let mut chunk = vec![];
        Read::by_ref(&mut input)
            .take(ARTIFACT_CHUNK_BYTES as u64)
            .read_to_end(&mut chunk)?;
        if chunk.is_empty() {
            break;
        }
        bytes += chunk.len() as u64;
        ensure!(
            bytes <= ARTIFACT_FILE_BYTES,
            "artifact grew beyond file limit"
        );
        whole.update(&chunk);
        chunks.push(BlobRef {
            digest: pvisor_cluster::artifacts::digest(&chunk),
            bytes: chunk.len() as u64,
        });
    }
    Ok(ArtifactFile {
        name: name.into(),
        bytes,
        digest: whole.finalize().to_hex().to_string(),
        chunks,
    })
}

fn prepare(
    storage: &Path,
    key: &LeaseKey,
    result: &pvisor_core::RunResult,
    retention: Option<&ArtifactRetention>,
    journal: Option<&pvisor::trace::Journal>,
    repository: Option<&checkpoints::Repository>,
    filesystem_pool: Option<&Path>,
) -> anyhow::Result<ArtifactManifest> {
    let names = retention.map_or_else(|| vec!["run-bundle.json"], ArtifactRetention::filenames);
    let sealed = storage.join("retained");
    if sealed.try_exists()? {
        ensure!(
            fs::symlink_metadata(&sealed)?.is_dir(),
            "retained spool is not a directory"
        );
        let manifest: ArtifactManifest = serde_json::from_slice(&read(
            &sealed.join("manifest.json"),
            ARTIFACT_CHUNK_BYTES as u64,
        )?)?;
        manifest.validate()?;
        ensure!(
            manifest.key == *key
                && manifest
                    .files
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    == names,
            "retained spool belongs to another lease or export request"
        );
        bundle_matches(&sealed.join("run-bundle.json"), result)?;
        return Ok(manifest);
    }
    let staging = storage.join(format!(".retention-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let prepared = (|| -> anyhow::Result<ArtifactManifest> {
        let bundle = bundle_matches(&storage.join("run-bundle.json"), result)?;
        copy(
            &storage.join("run-bundle.json"),
            &staging.join("run-bundle.json"),
        )?;
        if retention.is_some_and(|r| r.trace) {
            let destination = staging.join("trace");
            if let Some(journal) = journal {
                let mut output = create(&destination)?;
                journal.snapshot_to(&mut output, ARTIFACT_FILE_BYTES)?;
                seal(output)?;
            } else {
                copy(&storage.join("trace"), &destination)?;
            }
            pvisor::trace::Journal::validate(&destination)?;
        }
        if retention.is_some_and(|r| r.workspace_upper) {
            if result.state == pvisor_core::RunState::Hibernated {
                archive_suspended(
                    storage,
                    result,
                    &bundle,
                    &staging.join("workspace-upper.tar"),
                    filesystem_pool,
                )?;
            } else {
                archive_upper(storage, &bundle, &staging.join("workspace-upper.tar"))?;
            }
        }
        if let Some(requirement) =
            retention.and_then(|retention| retention.execution_checkpoint.as_ref())
        {
            let publication = repository
                .context("worker checkpoint repository is disabled")?
                .publish(storage, result, requirement)?;
            let output = create(&staging.join("execution-checkpoint.json"))?;
            serde_json::to_writer(&output, &publication)?;
            seal(output)?;
        }
        let manifest = ArtifactManifest {
            version: CLUSTER_VERSION,
            key: key.clone(),
            files: names
                .into_iter()
                .map(|name| describe(&staging, name))
                .collect::<anyhow::Result<_>>()?,
        };
        manifest.validate()?;
        super::persist(&staging.join("manifest.json"), &manifest)?;
        File::open(&staging)?.sync_all()?;
        fs::rename(&staging, &sealed)?;
        File::open(storage)?.sync_all()?;
        Ok(manifest)
    })();
    if prepared.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    prepared
}

/// Finish the blocking producer before reporting cancellation/completion. Lease
/// renewal remains asynchronous, but cancelled jobs cannot detach bulk copying
/// work and immediately admit an unbounded series of replacement producers.
pub(super) async fn seal_attempt(
    key: &LeaseKey,
    result: &pvisor_core::RunResult,
    storage: &Path,
    retention: Option<ArtifactRetention>,
    journal: Option<pvisor::trace::Journal>,
    repository: Option<Arc<checkpoints::Repository>>,
    filesystem_pool: Option<PathBuf>,
) -> anyhow::Result<ArtifactManifest> {
    let permit = if retention
        .as_ref()
        .is_some_and(|retention| retention.execution_checkpoint.is_some())
    {
        match &repository {
            Some(repository) => Some(repository.permit().await?),
            None => None,
        }
    } else {
        None
    };
    tokio::task::spawn_blocking({
        let storage = storage.to_owned();
        let key = key.clone();
        let result = result.clone();
        move || {
            let _permit = permit;
            prepare(
                &storage,
                &key,
                &result,
                retention.as_ref(),
                journal.as_ref(),
                repository.as_deref(),
                filesystem_pool.as_deref(),
            )
        }
    })
    .await?
}

/// Optional optimization: failures leave the worker's full reservation intact.
/// The normal completion protocol remains authoritative on older controllers.
pub(super) async fn handoff(
    client: &Client,
    key: &LeaseKey,
    result: &pvisor_core::RunResult,
) -> Option<pvisor_core::cluster::NativeDoneReceipt> {
    use pvisor_core::cluster::{ARTIFACT_DELIVERY_VERSION, NativeDone};
    match client
        .native_done(&NativeDone {
            version: ARTIFACT_DELIVERY_VERSION,
            key: key.clone(),
            result: result.clone(),
        })
        .await
    {
        Ok(receipt) => Some(receipt),
        Err(error) => {
            eprintln!(
                "artifact handoff unavailable; retaining local execution reservation: {error:#}"
            );
            None
        }
    }
}

pub(super) async fn upload(
    client: &Client,
    key: &LeaseKey,
    storage: &Path,
    manifest: ArtifactManifest,
) -> anyhow::Result<BlobRef> {
    for artifact in &manifest.files {
        let path = storage.join("retained").join(&artifact.name);
        let input = tokio::task::spawn_blocking(move || file(&path)).await??;
        ensure!(
            input.metadata()?.len() == artifact.bytes,
            "sealed artifact size changed"
        );
        let mut input = tokio::fs::File::from_std(input);
        let mut whole = blake3::Hasher::new();
        for chunk in &artifact.chunks {
            let mut bytes = vec![0; chunk.bytes as usize];
            input.read_exact(&mut bytes).await?;
            ensure!(
                pvisor_cluster::artifacts::digest(&bytes) == chunk.digest,
                "sealed artifact chunk changed"
            );
            whole.update(&bytes);
            ensure!(
                super::upload_retry(client, key, bytes).await? == *chunk,
                "artifact upload returned another chunk"
            );
        }
        ensure!(
            whole.finalize().to_hex().as_str() == artifact.digest,
            "sealed artifact digest changed"
        );
    }
    tokio::task::spawn_blocking({
        let storage = storage.to_owned();
        let manifest = manifest.clone();
        move || super::persist(&storage.join("artifact-manifest.json"), &manifest)
    })
    .await??;
    let reference = super::upload_retry(client, key, serde_json::to_vec(&manifest)?).await?;
    tokio::task::spawn_blocking({
        let storage = storage.to_owned();
        let reference = reference.clone();
        move || super::persist(&storage.join("artifact-reference.json"), &reference)
    })
    .await??;
    Ok(reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_os = "linux")]
    fn packed_snapshot_upper_streams_data_and_symlinks_without_a_materialized_tree() {
        use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("layer-001/env")).unwrap();
        fs::write(source.join("layer-001/env/answer"), b"private result").unwrap();
        fs::hard_link(
            source.join("layer-001/env/answer"),
            source.join("layer-001/env/alias"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            root.join("missing-external-secret"),
            source.join("layer-001/env/link"),
        )
        .unwrap();
        fs::write(source.join("layer-001/env/.wh.deleted"), b"").unwrap();
        let compatibility = Compatibility {
            host_boot: "boot".into(),
            build: "build".into(),
            firmware: "kernel".into(),
            profile: "profile".into(),
        };
        let store =
            SnapshotStore::with_filesystem_pool(&root.join("store"), &root.join("pool")).unwrap();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(b"RAM").unwrap();
        let id = pending
            .publish_chunked_filesystem(&source, b"machine", compatibility.clone(), false)
            .unwrap();
        fs::remove_dir_all(source).unwrap();
        let published = store.open(&id, &compatibility).unwrap();
        let destination = root.join("upper.tar");
        archive_snapshot_layer(&published, Path::new("layer-001"), &destination).unwrap();
        let mut found = std::collections::BTreeSet::new();
        for entry in tar::Archive::new(File::open(destination).unwrap())
            .entries()
            .unwrap()
        {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().into_owned();
            if name == Path::new("upper/env/answer") || name == Path::new("upper/env/alias") {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                assert_eq!(data, b"private result");
            }
            if name == Path::new("upper/env/link") {
                assert!(entry.header().entry_type().is_symlink());
            }
            found.insert(name);
        }
        for name in [
            "upper/env/answer",
            "upper/env/alias",
            "upper/env/link",
            "upper/env/.wh.deleted",
        ] {
            assert!(found.contains(Path::new(name)));
        }
        assert!(!root.join("store/objects").join(id).join("rootfs").exists());
    }
    #[test]
    fn upper_archive_preserves_writes_whiteouts_and_links_without_reading_link_targets() {
        let root = tempfile::tempdir().unwrap();
        let upper = root.path().join("upper");
        fs::create_dir_all(upper.join("env")).unwrap();
        fs::write(upper.join("env/answer"), b"private result").unwrap();
        fs::write(upper.join("env/.wh.deleted"), b"").unwrap();
        let outside = root.path().join("host-secret");
        fs::write(&outside, b"do-not-export-host-link-target").unwrap();
        std::os::unix::fs::symlink(&outside, upper.join("env/link")).unwrap();
        let destination = root.path().join("archive.tar");
        archive_tree(&upper, &destination).unwrap();
        let mut tar = tar::Archive::new(file(&destination).unwrap());
        let mut found = std::collections::BTreeSet::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().into_owned();
            if path == Path::new("upper/env/link") {
                assert!(entry.header().entry_type().is_symlink());
                assert_eq!(entry.link_name().unwrap().unwrap(), outside);
            }
            if path == Path::new("upper/env/answer") {
                let mut bytes = vec![];
                entry.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"private result");
            }
            found.insert(path);
        }
        assert!(found.contains(Path::new("upper/env/.wh.deleted")));
        assert!(found.contains(Path::new("upper/env/answer")));
        assert!(found.contains(Path::new("upper/env/link")));
        assert!(
            !fs::read(destination)
                .unwrap()
                .windows(b"do-not-export-host-link-target".len())
                .any(|w| w == b"do-not-export-host-link-target")
        );
    }
    #[test]
    fn upper_archive_rejects_special_files_and_oversized_sparse_outputs_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let upper = root.path().join("upper");
        fs::create_dir(&upper).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let fifo = upper.join("pipe");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(archive_tree(&upper, &root.path().join("fifo.tar")).is_err());
        fs::remove_file(fifo).unwrap();
        File::create(upper.join("huge"))
            .unwrap()
            .set_len(ARTIFACT_FILE_BYTES + 1)
            .unwrap();
        assert!(archive_tree(&upper, &root.path().join("huge.tar")).is_err());
        let mut writer = LimitedWriter {
            inner: vec![],
            remaining: 3,
        };
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert_eq!(writer.inner, b"abc");
    }
}

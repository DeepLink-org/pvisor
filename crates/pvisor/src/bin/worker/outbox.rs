//! Durable delivery of known terminal attempts. Never re-executes a command.
use anyhow::{Context, ensure};
use pvisor_cluster::{client::Client, *};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

const MAX_ENTRIES: usize = 4096;
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    worker_id: String,
    controller_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    version: u32,
    pub completion: Completion,
    pub retain_bundle: bool,
    pub ready: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    Accepted { phase: TaskPhase },
    Fenced,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    key: LeaseKey,
    pending_digest: String,
    disposition: Disposition,
}

pub struct Outbox {
    state: PathBuf,
    pending: PathBuf,
    receipts: PathBuf,
    binding: Binding,
    pending_index: Mutex<BTreeSet<String>>,
}

struct Entry {
    key: LeaseKey,
    pending_digest: String,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn key_name(key: &LeaseKey) -> String {
    format!(
        "{}.json",
        pvisor_cluster::artifacts::digest(&serde_json::to_vec(key).unwrap())
    )
}
fn digest(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(pvisor_cluster::artifacts::digest(&serde_json::to_vec(
        value,
    )?))
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => File::open(path.parent().context("missing parent")?)?.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "outbox directory must be private and cannot be a symlink"
    );
    Ok(())
}

fn read_optional<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_RECORD_BYTES,
        "outbox record is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECORD_BYTES,
        "outbox record grew beyond limit"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Atomic replace and directory fsync. A crash never truncates an existing record.
pub fn persist(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path.parent().context("missing record parent")?;
    let temporary = parent.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let written = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(value)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECORD_BYTES,
            "worker record exceeds durable limit"
        );
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(temporary);
    }
    written
}

impl Outbox {
    pub fn open(state: &Path, worker_id: &str, controller: &str) -> anyhow::Result<Self> {
        ensure!(identifier(worker_id), "invalid worker identity");
        let binding = Binding {
            version: CLUSTER_VERSION,
            worker_id: worker_id.into(),
            // Do not store URL credentials or query strings in plaintext.
            controller_digest: pvisor_cluster::artifacts::digest(
                controller.trim_end_matches('/').as_bytes(),
            ),
        };
        let root = state.join("outbox");
        private_directory(&root)?;
        let binding_path = root.join("binding.json");
        match read_optional::<Binding>(&binding_path)? {
            Some(previous) => ensure!(
                previous == binding,
                "worker state belongs to another worker/controller; use its original identity and URL"
            ),
            None => persist(&binding_path, &binding)?,
        }
        let pending = root.join("pending");
        let receipts = root.join("receipts");
        private_directory(&pending)?;
        private_directory(&receipts)?;
        let mut pending_index = BTreeSet::new();
        for entry in fs::read_dir(&pending)? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid outbox filename"))?;
            if !name.starts_with(".write-") {
                ensure!(
                    pending_index.len() < MAX_ENTRIES,
                    "pending outbox exceeds restart bound"
                );
                pending_index.insert(name);
            }
        }
        Ok(Self {
            state: state.into(),
            pending,
            receipts,
            binding,
            pending_index: Mutex::new(pending_index),
        })
    }

    fn validate(&self, pending: &Pending) -> anyhow::Result<()> {
        let completion = &pending.completion;
        let key = &completion.key;
        ensure!(
            pending.version == CLUSTER_VERSION
                && key.worker_id == self.binding.worker_id
                && identifier(&key.task_id)
                && identifier(&key.incarnation)
                && key.generation > 0,
            "invalid outbox attempt identity"
        );
        ensure!(
            completion.result.is_some() != completion.error.is_some(),
            "invalid terminal completion"
        );
        if let Some(result) = &completion.result {
            ensure!(
                matches!(
                    result.state,
                    pvisor_core::RunState::Completed
                        | pvisor_core::RunState::Failed
                        | pvisor_core::RunState::Cancelled
                ),
                "outbox cannot adopt nonterminal native execution"
            );
        }
        ensure!(
            pending.ready
                || (pending.retain_bundle
                    && completion.result.is_some()
                    && completion.artifacts.is_none()
                    && completion.artifact_error.is_none()),
            "only native terminal results may await export"
        );
        ensure!(
            !pending.ready
                || !pending.retain_bundle
                || completion.result.is_none()
                || completion.artifacts.is_some()
                || completion.artifact_error.is_some(),
            "ready completion is missing required export evidence"
        );
        ensure!(
            completion.artifacts.is_none() || completion.artifact_error.is_none(),
            "export reference and export error are mutually exclusive"
        );
        if let Some(reference) = &completion.artifacts {
            reference.validate()?;
        }
        Ok(())
    }

    pub fn save(
        &self,
        completion: &Completion,
        retain_bundle: bool,
        ready: bool,
    ) -> anyhow::Result<Pending> {
        let pending = Pending {
            version: CLUSTER_VERSION,
            completion: completion.clone(),
            retain_bundle,
            ready,
        };
        self.validate(&pending)?;
        let name = key_name(&completion.key);
        if let Some(receipt) = read_optional::<Receipt>(&self.receipts.join(&name))? {
            ensure!(
                receipt.version == CLUSTER_VERSION
                    && ready
                    && receipt.pending_digest == digest(&pending)?
                    && receipt.key == completion.key,
                "attempt already has a different delivery receipt"
            );
            return Ok(pending);
        }
        let path = self.pending.join(&name);
        if let Some(previous) = read_optional::<Pending>(&path)? {
            self.validate(&previous)?;
            if digest(&previous)? == digest(&pending)? {
                return Ok(pending);
            }
            ensure!(
                !previous.ready
                    && ready
                    && previous.retain_bundle == retain_bundle
                    && previous.completion.key == completion.key
                    && serde_json::to_value(&previous.completion.result)?
                        == serde_json::to_value(&completion.result)?
                    && previous.completion.error == completion.error,
                "conflicting or backwards outbox transition"
            );
        }
        let inserted = {
            let mut index = self
                .pending_index
                .lock()
                .map_err(|_| anyhow::anyhow!("outbox index poisoned"))?;
            ensure!(
                index.contains(&name) || index.len() < MAX_ENTRIES,
                "pending outbox is full"
            );
            index.insert(name.clone())
        };
        if let Err(error) = persist(&path, &pending) {
            // A rename followed by a failed directory fsync is uncertain. Keep
            // its index charge if the final file exists; do not over-admit.
            if inserted
                && fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                self.pending_index
                    .lock()
                    .map_err(|_| anyhow::anyhow!("outbox index poisoned"))?
                    .remove(&name);
            }
            return Err(error);
        }
        Ok(pending)
    }

    pub fn finish(&self, pending: &Pending, disposition: Disposition) -> anyhow::Result<()> {
        self.validate(pending)?;
        ensure!(pending.ready, "cannot acknowledge an unfinished export");
        if let Disposition::Accepted { phase } = disposition {
            ensure!(
                phase.terminal() && phase != TaskPhase::Lost,
                "invalid completion receipt phase"
            );
        }
        let receipt = Receipt {
            version: CLUSTER_VERSION,
            key: pending.completion.key.clone(),
            pending_digest: digest(pending)?,
            disposition,
        };
        let name = key_name(&receipt.key);
        let path = self.receipts.join(&name);
        if let Some(previous) = read_optional::<Receipt>(&path)? {
            ensure!(previous == receipt, "conflicting outbox delivery receipt");
        } else {
            persist(&path, &receipt)?;
        }
        match fs::remove_file(self.pending.join(&name)) {
            Ok(()) => File::open(&self.pending)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.pending_index
            .lock()
            .map_err(|_| anyhow::anyhow!("outbox index poisoned"))?
            .remove(&name);
        Ok(())
    }

    fn scan(&self) -> anyhow::Result<Vec<Entry>> {
        let mut records = Vec::new();
        for (count, entry) in fs::read_dir(&self.pending)?.enumerate() {
            ensure!(count < MAX_ENTRIES * 2, "too many pending outbox files");
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("invalid outbox filename")?;
            if name.starts_with(".write-") {
                continue;
            }
            let pending: Pending =
                read_optional(&entry.path())?.context("outbox file disappeared")?;
            self.validate(&pending)?;
            ensure!(
                name == key_name(&pending.completion.key),
                "outbox filename/key mismatch"
            );
            if let Some(receipt) = read_optional::<Receipt>(&self.receipts.join(name))? {
                ensure!(
                    receipt.version == CLUSTER_VERSION
                        && receipt.key == pending.completion.key
                        && receipt.pending_digest == digest(&pending)?,
                    "outbox receipt mismatch"
                );
                self.finish(&pending, receipt.disposition)?;
            } else {
                ensure!(
                    records.len() < MAX_ENTRIES,
                    "pending outbox exceeds restart bound"
                );
                // Retain only the small index. Large terminal output bodies
                // are decoded one at a time during delivery, not all at startup.
                records.push(Entry {
                    key: pending.completion.key.clone(),
                    pending_digest: digest(&pending)?,
                });
            }
        }
        Ok(records)
    }

    fn read(&self, entry: &Entry) -> anyhow::Result<Pending> {
        let pending: Pending = read_optional(&self.pending.join(key_name(&entry.key)))?
            .context("pending outbox disappeared during recovery")?;
        self.validate(&pending)?;
        ensure!(
            pending.completion.key == entry.key && digest(&pending)? == entry.pending_digest,
            "pending outbox changed during recovery"
        );
        Ok(pending)
    }

    #[cfg(test)]
    fn load(&self) -> anyhow::Result<Vec<Pending>> {
        self.scan()?.iter().map(|entry| self.read(entry)).collect()
    }

    fn storage(&self, key: &LeaseKey) -> PathBuf {
        self.state
            .join("tasks")
            .join(format!("{}-{}", key.task_id, key.generation))
    }
}

pub async fn save(
    outbox: Arc<Outbox>,
    completion: Completion,
    retain: bool,
    ready: bool,
) -> anyhow::Result<Pending> {
    tokio::task::spawn_blocking(move || outbox.save(&completion, retain, ready)).await?
}
pub async fn finish(
    outbox: Arc<Outbox>,
    pending: Pending,
    disposition: Disposition,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || outbox.finish(&pending, disposition)).await?
}

pub fn conflict(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(|e| e.status())
        == Some(reqwest::StatusCode::CONFLICT)
}
pub fn retryable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        e.status().is_none_or(|status| {
            status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        })
    })
}

#[derive(Clone)]
enum Lease {
    Unknown,
    Live { deadline: Instant },
    Stopped,
    Unavailable(String),
}
struct Pumps(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Pumps {
    fn drop(&mut self) {
        for pump in &self.0 {
            pump.abort();
        }
    }
}

async fn pump(client: Client, keys: Vec<(LeaseKey, watch::Sender<Lease>)>, poll_ms: u64) {
    let request = RecoveryRequest {
        worker_id: keys[0].0.worker_id.clone(),
        incarnation: keys[0].0.incarnation.clone(),
        completed: keys.iter().map(|(k, _)| k.clone()).collect(),
    };
    let mut interval = Duration::from_millis(poll_ms.min(1000));
    loop {
        let began = Instant::now();
        match client.recover(&request).await {
            Ok(response)
                if response.version == CLUSTER_VERSION
                    && (100..=300_000).contains(&response.lease_duration_ms)
                    && response
                        .renewed
                        .iter()
                        .chain(&response.stop)
                        .all(|key| request.completed.contains(key)) =>
            {
                interval = Duration::from_millis(poll_ms.min(response.lease_duration_ms / 4));
                for (key, tx) in &keys {
                    tx.send_replace(
                        if response.renewed.contains(key) && !response.stop.contains(key) {
                            Lease::Live {
                                deadline: began + Duration::from_millis(response.lease_duration_ms),
                            }
                        } else {
                            Lease::Stopped
                        },
                    );
                }
            }
            Ok(_) => {
                for (_, tx) in &keys {
                    tx.send_replace(Lease::Unavailable("invalid recovery lease response".into()));
                }
                break;
            }
            Err(e) if conflict(&e) => {
                for (_, tx) in &keys {
                    tx.send_replace(Lease::Stopped);
                }
                break;
            }
            Err(e) if retryable(&e) => eprintln!("outbox lease renewal failed: {e:#}"),
            Err(e) => {
                for (_, tx) in &keys {
                    tx.send_replace(Lease::Unavailable(format!("{e:#}")));
                }
                break;
            }
        }
        tokio::time::sleep(interval).await;
    }
}

async fn permitted(rx: &mut watch::Receiver<Lease>, initial: bool) -> anyhow::Result<()> {
    loop {
        let observed = rx.borrow().clone();
        match observed {
            Lease::Unknown => rx.changed().await.context("recovery renewal stopped")?,
            Lease::Live { deadline } => {
                ensure!(Instant::now() < deadline, "recovery artifact lease expired");
                if initial {
                    return Ok(());
                }
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => anyhow::bail!("recovery artifact lease expired"),
                    changed = rx.changed() => changed.context("recovery renewal stopped")?,
                }
            }
            Lease::Stopped => anyhow::bail!("recovery artifact lease stopped or fenced"),
            Lease::Unavailable(error) => anyhow::bail!("recovery protocol unavailable: {error}"),
        }
    }
}

pub async fn recover(outbox: Arc<Outbox>, client: Client, poll_ms: u64) -> anyhow::Result<()> {
    let entries = tokio::task::spawn_blocking({
        let outbox = outbox.clone();
        move || outbox.scan()
    })
    .await??;
    if entries.is_empty() {
        return Ok(());
    }
    eprintln!(
        "recovering {} durable terminal outbox records",
        entries.len()
    );
    let mut groups = BTreeMap::<String, Vec<(LeaseKey, watch::Sender<Lease>)>>::new();
    let mut receivers = BTreeMap::new();
    for entry in &entries {
        let key = &entry.key;
        let (tx, rx) = watch::channel(Lease::Unknown);
        groups
            .entry(key.incarnation.clone())
            .or_default()
            .push((key.clone(), tx));
        receivers.insert(key_name(key), rx);
    }
    let _pumps = Pumps(
        groups
            .into_values()
            .map(|keys| tokio::spawn(pump(client.clone(), keys, poll_ms)))
            .collect(),
    );
    for indexed in entries {
        let mut entry = tokio::task::spawn_blocking({
            let outbox = outbox.clone();
            move || outbox.read(&indexed)
        })
        .await??;
        if !entry.ready {
            let result = entry
                .completion
                .result
                .as_ref()
                .context("missing native terminal result")?;
            let mut rx = receivers.remove(&key_name(&entry.completion.key)).unwrap();
            // If the protocol is unavailable, retain NativeDone for an upgrade;
            // cancellation/expiry instead finalizes an explicit export failure.
            while matches!(*rx.borrow(), Lease::Unknown) {
                rx.changed().await?;
            }
            if let Lease::Unavailable(error) = rx.borrow().clone() {
                anyhow::bail!("{error}");
            }
            let storage = outbox.storage(&entry.completion.key);
            let exported = match permitted(&mut rx, true).await {
                Err(error) => Err(error),
                Ok(()) => tokio::select! {
                    exported = super::retain_bundle(&client, &entry.completion.key, result, &storage) => exported,
                    denied = permitted(&mut rx, false) => Err(denied.unwrap_err()),
                },
            };
            if let Lease::Unavailable(error) = rx.borrow().clone() {
                anyhow::bail!(
                    "recovery protocol unavailable; native export remains pending: {error}"
                );
            }
            match exported {
                Ok(reference) => entry.completion.artifacts = Some(reference),
                Err(error) => {
                    let mut message = Some(format!("{error:#}"));
                    super::bound_text(&mut message, &mut false, 8192);
                    entry.completion.artifact_error = message;
                }
            }
            entry = save(outbox.clone(), entry.completion, entry.retain_bundle, true).await?;
            let local = entry.completion.clone();
            tokio::task::spawn_blocking(move || persist(&storage.join("completion.json"), &local))
                .await??;
        }
        let mut delay = Duration::from_millis(50);
        let disposition = loop {
            match client.complete(&entry.completion).await {
                Ok(task) => break Disposition::Accepted { phase: task.phase },
                Err(error) if conflict(&error) => {
                    eprintln!(
                        "recovered completion for {} was fenced; retaining local native evidence",
                        entry.completion.key.task_id
                    );
                    break Disposition::Fenced;
                }
                Err(error) if retryable(&error) => {
                    eprintln!("recovered completion delivery failed: {error:#}");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
                Err(error) => return Err(error.context("outbox delivery remains pending")),
            }
        };
        finish(outbox.clone(), entry, disposition).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> Completion {
        Completion {
            key: LeaseKey {
                task_id: "task".into(),
                worker_id: "worker".into(),
                incarnation: "epoch".into(),
                generation: 1,
            },
            result: Some(
                serde_json::from_value(serde_json::json!({
                    "run_id": "run", "attempt_id": "attempt", "state": "completed",
                    "started_at_unix_ms": 1, "finished_at_unix_ms": 2, "exit_code": 0
                }))
                .unwrap(),
            ),
            error: None,
            artifacts: None,
            artifact_error: None,
        }
    }

    #[test]
    fn restart_preserves_native_result_export_transition_and_receipt_crash_window() {
        let temp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(temp.path(), "worker", "http://controller/").unwrap();
        let native = terminal();
        outbox.save(&native, true, false).unwrap();
        let outbox = Outbox::open(temp.path(), "worker", "http://controller").unwrap();
        let entries = outbox.load().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].ready);
        assert_eq!(
            serde_json::to_value(&entries[0].completion.result).unwrap(),
            serde_json::to_value(&native.result).unwrap()
        );
        assert!(outbox.finish(&entries[0], Disposition::Fenced).is_err());
        let mut exported = native.clone();
        exported.artifact_error = Some("explicit export failure".into());
        let ready = outbox.save(&exported, true, true).unwrap();
        assert!(outbox.save(&native, true, false).is_err());
        let mut conflicting = exported.clone();
        conflicting.result.as_mut().unwrap().exit_code = Some(5);
        assert!(outbox.save(&conflicting, true, true).is_err());
        let accepted = Disposition::Accepted {
            phase: TaskPhase::Failed,
        };
        outbox.finish(&ready, accepted.clone()).unwrap();
        // Crash after receipt fsync but before unlink: replay consumes the
        // matching pending entry without another HTTP operation.
        persist(&outbox.pending.join(key_name(&native.key)), &ready).unwrap();
        assert!(outbox.load().unwrap().is_empty());
        outbox.finish(&ready, accepted).unwrap();
        assert!(outbox.finish(&ready, Disposition::Fenced).is_err());
        assert!(outbox.save(&exported, true, true).is_ok());
        assert!(outbox.load().unwrap().is_empty());
    }

    #[test]
    fn binding_corruption_symlinks_and_nonterminal_attempts_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(temp.path(), "worker", "http://controller").unwrap();
        assert!(Outbox::open(temp.path(), "other", "http://controller").is_err());
        assert!(Outbox::open(temp.path(), "worker", "http://another").is_err());
        let mut value = terminal();
        value.result.as_mut().unwrap().state = pvisor_core::RunState::Running;
        assert!(outbox.save(&value, true, false).is_err());
        value = terminal();
        value.key.task_id = "../escape".into();
        assert!(outbox.save(&value, true, false).is_err());
        let native = terminal();
        let path = outbox.pending.join(key_name(&native.key));
        let secret = temp.path().join("secret");
        fs::write(&secret, "unchanged").unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        assert!(outbox.load().is_err());
        assert!(outbox.save(&native, true, false).is_err());
        assert_eq!(fs::read_to_string(&secret).unwrap(), "unchanged");
        fs::remove_file(&path).unwrap();
        fs::write(&path, "{partial").unwrap();
        assert!(outbox.load().is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "{partial");
    }

    #[test]
    fn interrupted_atomic_write_keeps_previous_record_and_ignores_unpublished_temp() {
        struct Broken;
        impl Serialize for Broken {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(temp.path(), "worker", "http://controller").unwrap();
        let native = terminal();
        outbox.save(&native, true, false).unwrap();
        let path = outbox.pending.join(key_name(&native.key));
        let before = fs::read(&path).unwrap();
        assert!(persist(&path, &Broken).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
        fs::write(
            outbox.pending.join(".write-crash"),
            "partial unpublished bytes",
        )
        .unwrap();
        assert_eq!(outbox.load().unwrap().len(), 1);
    }

    #[test]
    fn full_restarted_queue_refuses_another_record_until_a_receipt_releases_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let original = Outbox::open(temp.path(), "worker", "http://controller").unwrap();
        for index in 0..MAX_ENTRIES {
            let mut completion = terminal();
            completion.key.task_id = format!("task-{index}");
            let entry = Pending {
                version: CLUSTER_VERSION,
                completion,
                retain_bundle: false,
                ready: true,
            };
            fs::write(
                original.pending.join(key_name(&entry.completion.key)),
                serde_json::to_vec(&entry).unwrap(),
            )
            .unwrap();
        }
        drop(original);
        let restarted = Outbox::open(temp.path(), "worker", "http://controller").unwrap();
        let entries = restarted.load().unwrap();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert!(restarted.save(&terminal(), false, true).is_err());
        assert!(!restarted.pending.join(key_name(&terminal().key)).exists());
        restarted
            .finish(
                &entries[0],
                Disposition::Accepted {
                    phase: TaskPhase::Succeeded,
                },
            )
            .unwrap();
        restarted.save(&terminal(), false, true).unwrap();
        assert_eq!(restarted.load().unwrap().len(), MAX_ENTRIES);
    }
}

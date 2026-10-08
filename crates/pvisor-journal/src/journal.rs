use anyhow::{Context as _, Result, ensure};
use fs2::FileExt;
use pvisor_core::event::{Durability, Event, MAX_EVENT_BYTES, Position, Receipt, Record, VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{Arc, Mutex},
};

use crate::api::{AppendError, DurableFiles, JournalStore, Persistence};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    journal: String,
}

struct State {
    id: String,
    file: Option<File>,
    // Retains one digest per event for duplicate detection.
    seen: BTreeMap<String, (u64, [u8; 32])>,
    causes: BTreeMap<String, Vec<String>>,
    unresolved: std::collections::BTreeSet<String>,
    memory: Vec<Record>,
    poisoned: bool,
}

/// Opaque shared storage owner; see [`JournalStore`] for lifecycle and locking.
#[derive(Clone)]
pub struct Journal {
    state: Arc<Mutex<State>>,
    live: tokio::sync::broadcast::Sender<Event>,
}

impl Default for Journal {
    fn default() -> Self {
        Self::memory()
    }
}

impl JournalStore for Journal {
    /// Live events are published only after commit. Retry receipts do not
    /// produce duplicate notifications; lagging readers recover from the journal.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.live.subscribe()
    }

    /// Inspect a closed journal without repairing or changing it.
    fn read(path: &Path) -> Result<Vec<Record>> {
        let mut file = File::open(path)?;
        FileExt::try_lock_shared(&file)
            .context("close the trace writer before inspecting its journal")?;
        Ok(scan(&mut file, false, true)?.1)
    }

    /// Validate a closed durable journal without retaining event payloads.
    fn validate(path: &Path) -> Result<()> {
        let mut file = File::open(path)?;
        FileExt::try_lock_shared(&file).context("close the trace writer before validation")?;
        scan(&mut file, false, false)?;
        Ok(())
    }

    fn memory() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                id: uuid::Uuid::new_v4().to_string(),
                file: None,
                seen: BTreeMap::new(),
                causes: BTreeMap::new(),
                unresolved: Default::default(),
                memory: Vec::new(),
                poisoned: false,
            })),
            live: tokio::sync::broadcast::channel(256).0,
        }
    }

    fn open(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Persistence::create_dir_all_durable(parent)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.try_lock_exclusive()
            .context("trace journal already has a writer")?;
        let (id, records) = if file.metadata()?.len() == 0 {
            let id = uuid::Uuid::new_v4().to_string();
            let header = Header {
                format: format!("pvisor.trace/{VERSION}"),
                journal: id.clone(),
            };
            serde_json::to_writer(&mut file, &header)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Persistence::sync_directory(parent)?;
            (id, Vec::new())
        } else {
            scan(&mut file, true, true)?
        };
        // A full record left by a lost acknowledgement is durable before retry
        // can return a LocalSync receipt, even when its old sync failed.
        file.sync_all()?;
        let mut seen = BTreeMap::new();
        let mut causes = BTreeMap::new();
        for record in records {
            let digest = digest(&record.event)?;
            causes.insert(record.event.id.clone(), record.event.caused_by.clone());
            ensure!(
                seen.insert(record.event.id, (record.position.offset, digest))
                    .is_none(),
                "duplicate event in journal"
            );
        }
        let unresolved = causes
            .values()
            .flatten()
            .filter(|id| !causes.contains_key(*id))
            .cloned()
            .collect();
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                id,
                file: Some(file),
                seen,
                causes,
                unresolved,
                memory: Vec::new(),
                poisoned: false,
            })),
            live: tokio::sync::broadcast::channel(256).0,
        })
    }

    fn append(&self, event: Event) -> std::result::Result<Receipt, AppendError> {
        event
            .validate()
            .map_err(|e| AppendError::Rejected(e.to_string()))?;
        let hash = digest(&event).map_err(|e| AppendError::Rejected(e.to_string()))?;
        let mut state = self
            .state
            .lock()
            .map_err(|e| AppendError::Unknown(e.to_string()))?;
        if state.poisoned {
            return Err(AppendError::Unknown("previous write failed".into()));
        }
        let durability = if state.file.is_some() {
            Durability::LocalSync
        } else {
            Durability::Volatile
        };
        if let Some((offset, prior)) = state.seen.get(&event.id) {
            if prior != &hash {
                return Err(AppendError::Rejected(
                    "event identity has different content".into(),
                ));
            }
            return Ok(Receipt {
                event: event.id,
                position: Position {
                    journal: state.id.clone(),
                    offset: *offset,
                },
                durability,
            });
        }
        // A new node can close a cycle only when a previous record referenced it.
        // Normal append remains O(edges), including arbitrarily long trace chains.
        if state.unresolved.contains(&event.id) {
            let mut pending = event.caused_by.clone();
            let mut visited = std::collections::BTreeSet::new();
            while let Some(id) = pending.pop() {
                if id == event.id {
                    return Err(AppendError::Rejected("causal cycle".into()));
                }
                if visited.insert(id.clone())
                    && let Some(edges) = state.causes.get(&id)
                {
                    pending.extend(edges.iter().cloned());
                }
            }
        }
        let offset =
            u64::try_from(state.seen.len()).map_err(|e| AppendError::Rejected(e.to_string()))?;
        let record = Record {
            position: Position {
                journal: state.id.clone(),
                offset,
            },
            event,
        };
        let mut bytes =
            serde_json::to_vec(&record).map_err(|e| AppendError::Rejected(e.to_string()))?;
        bytes.push(b'\n');
        if let Some(file) = state.file.as_mut() {
            let result = (|| -> std::io::Result<()> {
                file.seek(SeekFrom::End(0))?;
                file.write_all(&bytes)?;
                file.sync_all()
            })();
            if let Err(error) = result {
                state.poisoned = true;
                return Err(AppendError::Unknown(error.to_string()));
            }
        } else {
            state.memory.push(record.clone());
        }
        state.seen.insert(record.event.id.clone(), (offset, hash));
        state.unresolved.remove(&record.event.id);
        for id in &record.event.caused_by {
            if !state.seen.contains_key(id) {
                state.unresolved.insert(id.clone());
            }
        }
        state
            .causes
            .insert(record.event.id.clone(), record.event.caused_by.clone());
        let _ = self.live.send(record.event.clone());
        Ok(Receipt {
            event: record.event.id,
            position: record.position,
            durability,
        })
    }

    async fn append_async(&self, event: Event) -> std::result::Result<Receipt, AppendError> {
        let journal = self.clone();
        tokio::task::spawn_blocking(move || journal.append(event))
            .await
            .map_err(|e| AppendError::Unknown(e.to_string()))?
    }

    fn records(&self) -> Result<Vec<Record>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock poisoned"))?;
        ensure!(!state.poisoned, "reopen journal after write error");
        if let Some(file) = state.file.as_mut() {
            Ok(scan(file, false, true)?.1)
        } else {
            Ok(state.memory.clone())
        }
    }

    /// Copy committed durable bytes while excluding concurrent appenders.
    /// The caller owns publication/fsync of the destination. Never exports a
    /// poisoned or volatile journal, and never repairs incomplete data.
    fn snapshot_to(&self, output: &mut impl Write, max_bytes: u64) -> Result<u64> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock poisoned"))?;
        ensure!(!state.poisoned, "reopen journal after write error");
        let file = state
            .file
            .as_mut()
            .context("durable trace export requires a file journal")?;
        let size = file.metadata()?.len();
        ensure!(size <= max_bytes, "trace exceeds artifact export limit");
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let copied = std::io::copy(&mut file.take(size), output)?;
        ensure!(copied == size, "trace changed while snapshotting");
        Ok(copied)
    }
}

fn digest(event: &Event) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(event)?).into())
}

fn scan(file: &mut File, repair_tail: bool, retain_records: bool) -> Result<(String, Vec<Record>)> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(&mut *file);
    let mut line = Vec::new();
    read_line(&mut reader, &mut line)?;
    ensure!(line.last() == Some(&b'\n'), "incomplete trace header");
    let header: Header = serde_json::from_slice(&line).context("not a trace journal")?;
    ensure!(
        header.format == format!("pvisor.trace/{VERSION}")
            && !header.journal.is_empty()
            && header.journal.len() <= 256,
        "unsupported journal header"
    );
    let mut valid_bytes = line.len() as u64;
    let mut records = Vec::new();
    let mut ids = BTreeMap::new();
    let mut offset = 0_u64;
    loop {
        line.clear();
        if read_line(&mut reader, &mut line)? == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            ensure!(repair_tail, "incomplete trace tail");
            break;
        }
        let record: Record =
            serde_json::from_slice(&line).context("corrupt complete trace record")?;
        record.event.validate()?;
        ensure!(
            record.position.journal == header.journal && record.position.offset == offset,
            "trace position discontinuity"
        );
        ensure!(
            ids.insert(record.event.id.clone(), record.event.caused_by.clone())
                .is_none(),
            "duplicate event identity"
        );
        // References to an earlier event can resolve an older forward reference.
        // Check the resulting graph, not timestamps or submission order.
        if retain_records {
            records.push(record);
        }
        offset += 1;
        valid_bytes += line.len() as u64;
    }
    drop(reader);
    validate_causality(&ids)?;
    if repair_tail && file.metadata()?.len() != valid_bytes {
        file.set_len(valid_bytes)?;
        file.sync_all()?;
    }
    Ok((header.journal, records))
}

fn read_line(reader: &mut impl BufRead, bytes: &mut Vec<u8>) -> Result<usize> {
    let size = reader
        .take((MAX_EVENT_BYTES + 4097) as u64)
        .read_until(b'\n', bytes)?;
    ensure!(
        size <= MAX_EVENT_BYTES + 4096,
        "trace record exceeds size limit"
    );
    Ok(size)
}

fn validate_causality(graph: &BTreeMap<String, Vec<String>>) -> Result<()> {
    // Iterative DFS avoids a stack overflow on long causal chains.
    let mut colors = BTreeMap::<&str, u8>::new();
    for start in graph.keys() {
        let mut stack = vec![(start.as_str(), false)];
        while let Some((id, exiting)) = stack.pop() {
            if exiting {
                colors.insert(id, 2);
                continue;
            }
            match colors.get(id) {
                Some(1) => anyhow::bail!("causal cycle at {id}"),
                Some(2) => continue,
                _ => {}
            }
            colors.insert(id, 1);
            stack.push((id, true));
            if let Some(edges) = graph.get(id) {
                stack.extend(
                    edges
                        .iter()
                        .filter(|id| graph.contains_key(*id))
                        .map(|id| (id.as_str(), false)),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Trace, TraceProducer};
    use pvisor_core::event::Fact;
    use std::{
        future::Future,
        task::{Context, Waker},
    };

    #[test]
    fn durable_snapshot_can_be_read_with_a_live_writer_and_excludes_later_facts() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("trace");
        let journal = Journal::open(&source).unwrap();
        let trace = Trace::new(journal.clone(), "snapshot-test");
        let event = || {
            trace.event(
                vec!["run".into()],
                None,
                None,
                vec![],
                Fact::Observation {
                    domain: "test".into(),
                    name: "step".into(),
                    version: 1,
                    payload: serde_json::json!({"completed":true}),
                },
            )
        };
        journal.append(event()).unwrap();
        journal.append(event()).unwrap();
        let snapshot = root.path().join("snapshot");
        let mut output = File::create(&snapshot).unwrap();
        assert!(journal.snapshot_to(&mut output, 0).is_err());
        assert_eq!(output.metadata().unwrap().len(), 0);
        let size = journal.snapshot_to(&mut output, 1024 * 1024).unwrap();
        output.sync_all().unwrap();
        assert_eq!(size, std::fs::metadata(&source).unwrap().len());
        assert_eq!(
            std::fs::read(&snapshot).unwrap(),
            std::fs::read(&source).unwrap()
        );
        journal.append(event()).unwrap();
        assert_eq!(Journal::read(&snapshot).unwrap().len(), 2);
        Journal::validate(&snapshot).unwrap();
        let good = std::fs::read(&snapshot).unwrap();
        let shifted = String::from_utf8(good.clone())
            .unwrap()
            .replacen("\"offset\":0", "\"offset\":1", 1)
            .into_bytes();
        let bad_version = String::from_utf8(good.clone())
            .unwrap()
            .replace(&format!("pvisor.trace/{VERSION}"), "pvisor.trace/0")
            .into_bytes();
        for (index, corrupt) in [good[..good.len() - 1].to_vec(), shifted, bad_version]
            .into_iter()
            .enumerate()
        {
            let path = root.path().join(format!("corrupt-{index}"));
            std::fs::write(&path, corrupt).unwrap();
            assert!(Journal::read(&path).is_err());
            assert!(Journal::validate(&path).is_err());
        }
        assert!(Journal::validate(&source).is_err());
        assert_eq!(journal.records().unwrap().len(), 3);
        assert!(Journal::read(&source).is_err());
        assert!(
            Journal::memory()
                .snapshot_to(&mut Vec::new(), 1024)
                .is_err()
        );
        journal.state.lock().unwrap().poisoned = true;
        assert!(journal.snapshot_to(&mut Vec::new(), 1024 * 1024).is_err());
    }

    #[test]
    fn write_error_requires_recovery_before_another_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let journal = Journal::open(&path).unwrap();
        let trace = Trace::new(journal.clone(), "fault-test");
        let event = trace.event(
            vec!["test".into()],
            None,
            None,
            vec![],
            Fact::Observation {
                domain: "runtime".into(),
                name: "test".into(),
                version: 1,
                payload: serde_json::Value::Null,
            },
        );
        // Inject a write failure with a read-only descriptor, without changing bytes.
        journal.state.lock().unwrap().file = Some(File::open(&path).unwrap());
        assert!(matches!(
            journal.append(event.clone()),
            Err(AppendError::Unknown(_))
        ));
        assert!(matches!(
            journal.append(event.clone()),
            Err(AppendError::Unknown(_))
        ));
        assert!(journal.records().is_err());
        drop(trace);
        drop(journal);
        let recovered = Journal::open(&path).unwrap();
        let receipt = recovered.append(event).unwrap();
        assert_eq!(receipt.position.offset, 0);
        assert_eq!(receipt.durability, Durability::LocalSync);
    }

    #[tokio::test]
    async fn cancelling_waiter_does_not_cancel_accepted_append() {
        let trace = Trace::new(Journal::memory(), "test");
        let event = trace.event(
            vec!["runtime:test".into()],
            None,
            None,
            vec![],
            Fact::Observation {
                domain: "runtime".into(),
                name: "test".into(),
                version: 1,
                payload: serde_json::Value::Null,
            },
        );
        {
            let _guard = trace.journal().state.lock().unwrap();
            let mut waiting = Box::pin(trace.journal().append_async(event.clone()));
            let poll = waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            assert!(poll.is_pending()); // spawn_blocking owns the append, blocked on the lock.
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if !trace.journal().records().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let receipt = trace.journal().append_async(event).await.unwrap();
        assert_eq!(receipt.position.offset, 0);
        assert_eq!(trace.journal().records().unwrap().len(), 1);
    }
}

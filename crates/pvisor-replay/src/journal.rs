//! Replay safety observations use the shared fact journal.
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use fs2::FileExt;
use pvisor_core::event::Fact;
use pvisor_journal::{Journal as FactJournal, Trace};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub struct Journal {
    _lock: File,
    trace: Trace,
    cause: Option<String>,
    pub path: PathBuf,
}
impl Journal {
    pub fn open(state_dir: &Path) -> Result<Self, ReplayError> {
        fs::create_dir_all(state_dir)
            .replay_context(ReplayErrorKind::Executor, "create replay state")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state_dir.join("run.lock"))
            .replay_context(ReplayErrorKind::Executor, "open replay lock")?;
        lock.try_lock_exclusive().map_err(|e| {
            ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                format!("another replay process owns state: {e}"),
            )
        })?;
        let path = state_dir.join("replay-events.jsonl");
        // Recover an incomplete tail before checking tool execution safety.
        let journal = FactJournal::open(&path)
            .replay_context(ReplayErrorKind::Executor, "open replay fact journal")?;
        let events = journal
            .records()
            .replay_context(ReplayErrorKind::AmbiguousExecution, "recover replay facts")?
            .into_iter()
            .map(|record| observation(record.event))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(call_id) = ambiguous(events.into_iter().flatten()) {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                format!(
                    "state contains an uncertain started tool call {call_id:?}; use a new sandbox and run-id to replay from T1"
                ),
            ));
        }
        Ok(Self {
            _lock: lock,
            trace: Trace::new(journal, "pvisor-replay"),
            cause: None,
            path,
        })
    }
    pub fn append(
        &mut self,
        name: &str,
        fields: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), ReplayError> {
        let payload = Value::Object(fields.into_iter().collect::<Map<_, _>>());
        let event = self.trace.event(
            vec!["replay".into(), self.trace.id.clone()],
            None,
            None,
            self.cause.iter().cloned().collect(),
            Fact::Observation {
                domain: "replay".into(),
                name: name.into(),
                version: 1,
                payload,
            },
        );
        let receipt = self
            .trace
            .journal
            .append(event)
            .replay_context(ReplayErrorKind::Executor, "commit replay fact")?;
        self.cause = Some(receipt.event);
        Ok(())
    }
    #[cfg(test)]
    fn find_ambiguous(path: &Path) -> Result<Option<String>, ReplayError> {
        Ok(ambiguous(read_observations(path)?))
    }
}

fn ambiguous(events: impl IntoIterator<Item = Value>) -> Option<String> {
    let mut started = BTreeSet::new();
    for event in events {
        match event["event"].as_str() {
            Some("tool_started") => {
                if let Some(call_id) = event["call_id"].as_str() {
                    started.insert(call_id.to_owned());
                }
            }
            Some("run_finished" | "run_failed") => started.clear(),
            _ => {}
        }
    }
    started.into_iter().next()
}
fn observation(event: pvisor_core::event::Event) -> Result<Option<Value>, ReplayError> {
    if let Fact::Observation {
        domain,
        name,
        version,
        mut payload,
    } = event.data
        && domain == "replay"
    {
        if version != 1 {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                "unsupported replay observation version",
            ));
        }
        payload["event"] = Value::String(name);
        return Ok(Some(payload));
    }
    Ok(None)
}

/// Read committed replay observations for diagnostics while the writer is open.
pub(crate) fn read_observations(path: &Path) -> Result<Vec<Value>, ReplayError> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let file = File::open(path)
        .replay_context(ReplayErrorKind::AmbiguousExecution, "read replay facts")?;
    let mut lines = BufReader::new(file).lines();
    let Some(first) = lines.next() else {
        return Ok(vec![]);
    };
    let first = first.replay_context(ReplayErrorKind::AmbiguousExecution, "read replay header")?;
    let header: Value = serde_json::from_str(&first)
        .replay_context(ReplayErrorKind::AmbiguousExecution, "parse replay header")?;
    if header["format"] != format!("pvisor.trace/{}", pvisor_core::event::VERSION)
        || header["journal"]
            .as_str()
            .is_none_or(|id| id.is_empty() || id.len() > 256)
    {
        return Err(ReplayError::new(
            ReplayErrorKind::AmbiguousExecution,
            "unsupported replay journal header",
        ));
    }
    // Admission uses the recovering Journal reader; failure reporting can
    // inspect complete records while the owning handle remains open.
    let mut out = vec![];
    for line in lines {
        let line = line.replay_context(ReplayErrorKind::AmbiguousExecution, "read replay fact")?;
        let record: pvisor_core::event::Record = serde_json::from_str(&line)
            .replay_context(ReplayErrorKind::AmbiguousExecution, "decode replay fact")?;
        record
            .event
            .validate()
            .replay_context(ReplayErrorKind::AmbiguousExecution, "validate replay fact")?;
        if let Some(value) = observation(record.event)? {
            out.push(value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_jsonl_is_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        let old = "{\"event\":\"run_started\"}\n";
        fs::write(&path, old).unwrap();
        assert!(read_observations(&path).is_err());
        assert!(Journal::open(temporary.path()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), old);
    }

    fn write_events(path: &Path, events: &[Value]) {
        let mut journal = Journal::open(path.parent().unwrap()).unwrap();
        for value in events {
            let mut fields = value.as_object().unwrap().clone();
            let name = fields.remove("event").unwrap();
            journal.append(name.as_str().unwrap(), fields).unwrap();
        }
    }

    #[test]
    fn finished_tool_without_terminal_run_is_ambiguous() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
                serde_json::json!({"event": "tool_finished", "call_id": "same-call"}),
            ],
        );

        assert_eq!(
            Journal::find_ambiguous(&path).unwrap().as_deref(),
            Some("same-call")
        );
    }

    #[test]
    fn repeated_call_id_after_a_terminal_run_remains_ambiguous() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
                serde_json::json!({"event": "tool_finished", "call_id": "same-call"}),
                serde_json::json!({"event": "run_finished"}),
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
            ],
        );

        assert_eq!(
            Journal::find_ambiguous(&path).unwrap().as_deref(),
            Some("same-call")
        );
    }

    #[test]
    fn interruption_before_any_tool_starts_is_retryable() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "plan_validated"}),
            ],
        );

        assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
    }

    #[test]
    fn failed_run_is_terminal() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "call-1"}),
                serde_json::json!({"event": "run_failed"}),
            ],
        );

        assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
    }

    #[test]
    fn opening_a_journal_rejects_ambiguous_state_while_holding_the_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "call-1"}),
            ],
        );

        let error = Journal::open(temporary.path()).err().unwrap();

        assert_eq!(error.kind, ReplayErrorKind::AmbiguousExecution);
        assert!(error.message.contains("call-1"));
    }
}

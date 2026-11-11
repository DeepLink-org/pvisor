//! Replay safety observations use the shared fact journal.
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use fs2::FileExt;
use pvisor_core::event::Fact;
use pvisor_journal::api::{JournalStore, Trace, TraceProducer};
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
        let journal = pvisor_journal::api::Journal::open(&path)
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
            vec!["replay".into(), self.trace.id().to_owned()],
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
            .journal()
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

pub(crate) fn ambiguous(events: impl IntoIterator<Item = Value>) -> Option<String> {
    let mut started = BTreeSet::new();
    let mut finished = BTreeSet::new();
    for event in events {
        match event["event"].as_str() {
            Some("tool_started") => {
                if let Some(call_id) = event["call_id"].as_str() {
                    // Native trajectories may reuse IDs across turns. A prior
                    // operation's finish cannot resolve this new execution.
                    finished.remove(call_id);
                    started.insert(call_id.to_owned());
                }
            }
            Some("tool_finished") => {
                if let Some(call_id) = event["call_id"].as_str() {
                    finished.insert(call_id.to_owned());
                }
            }
            Some("run_finished" | "run_failed") => {
                // A terminal run receipt cannot establish an unfinished tool's
                // outcome. Retain its uncertainty across failures and restarts.
                started.retain(|call_id| !finished.contains(call_id));
                finished.clear();
            }
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
        payload,
    } = event.data
        && domain == "replay"
    {
        if version != 1 {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                "unsupported replay observation version",
            ));
        }
        let Value::Object(mut fields) = payload else {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                "replay observation payload must be an object",
            ));
        };
        if matches!(name.as_str(), "tool_started" | "tool_finished")
            && fields.get("call_id").and_then(Value::as_str).is_none()
        {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                "replay observation tool receipt requires a string call_id",
            ));
        }
        fields.insert("event".into(), Value::String(name));
        return Ok(Some(Value::Object(fields)));
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

    #[test]
    fn malformed_receipts_reject_without_mutation_then_allow_explicit_restore() {
        use pvisor_journal::api::Journal as FactJournal;

        let invalid = [
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!(7),
            serde_json::json!("call-1"),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"call_id": null}),
            serde_json::json!({"call_id": 7}),
        ];
        for name in ["tool_started", "tool_finished"] {
            for payload in &invalid {
                let temporary = tempfile::tempdir().unwrap();
                let path = temporary.path().join("replay-events.jsonl");
                let journal = FactJournal::open(&path).unwrap();
                let healthy = fs::read(&path).unwrap();
                let trace = Trace::new(journal, "receipt-fault-fixture");
                trace
                    .journal()
                    .append(trace.event(
                        vec!["replay".into()],
                        None,
                        None,
                        vec![],
                        Fact::Observation {
                            domain: "replay".into(),
                            name: name.into(),
                            version: 1,
                            payload: payload.clone(),
                        },
                    ))
                    .unwrap();
                drop(trace);
                let damaged = fs::read(&path).unwrap();
                for _ in 0..2 {
                    let error = Journal::open(temporary.path())
                        .err()
                        .unwrap_or_else(|| panic!("accepted malformed {name} receipt: {payload}"));
                    assert_eq!(error.kind, ReplayErrorKind::AmbiguousExecution);
                    assert!(error.message.contains("replay observation"));
                    assert_eq!(fs::read(&path).unwrap(), damaged);
                    assert!(read_observations(&path).is_err());
                }
                // Only the owner restores the pre-fault backup. Rejection
                // must release both locks and leave subsequent use healthy.
                fs::write(&path, &healthy).unwrap();
                let mut recovered = Journal::open(temporary.path()).unwrap();
                // Codex and OpenCode currently accept empty string native
                // IDs. Those still participate in start/finish tracking.
                for call_id in ["", "healthy"] {
                    for event in [
                        serde_json::json!({"event":"tool_started", "call_id":call_id}),
                        serde_json::json!({"event":"tool_finished", "call_id":call_id}),
                        serde_json::json!({"event":"run_finished"}),
                    ] {
                        let mut fields = event.as_object().unwrap().clone();
                        let name = fields.remove("event").unwrap();
                        recovered.append(name.as_str().unwrap(), fields).unwrap();
                    }
                }
                drop(recovered);
                assert!(Journal::open(temporary.path()).is_ok());
                assert_eq!(read_observations(&path).unwrap().len(), 6);
            }
        }
    }

    #[test]
    fn receipt_validation_preserves_other_domains_and_open_observation_fields() {
        let trace = Trace::new(pvisor_journal::api::Journal::memory(), "scope-fixture");
        for payload in [
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"call_id": 7}),
        ] {
            let event = trace.event(
                vec![],
                None,
                None,
                vec![],
                Fact::Observation {
                    domain: "unrelated".into(),
                    name: "tool_started".into(),
                    version: 99,
                    payload,
                },
            );
            assert_eq!(observation(event).unwrap(), None);
        }
        let event = trace.event(
            vec![],
            None,
            None,
            vec![],
            Fact::Observation {
                domain: "replay".into(),
                name: "future_diagnostic".into(),
                version: 1,
                payload: serde_json::json!({"call_id": 7, "extra": [true]}),
            },
        );
        assert_eq!(
            observation(event).unwrap(),
            Some(serde_json::json!({
                "event": "future_diagnostic", "call_id": 7, "extra": [true]
            }))
        );
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
    fn failed_run_does_not_resolve_an_unfinished_tool() {
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

        assert_eq!(
            Journal::find_ambiguous(&path).unwrap().as_deref(),
            Some("call-1")
        );
        assert_eq!(
            Journal::open(temporary.path()).err().unwrap().kind,
            ReplayErrorKind::AmbiguousExecution
        );
    }

    #[test]
    fn terminal_run_with_finished_tool_is_retryable() {
        for terminal in ["run_finished", "run_failed"] {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("replay-events.jsonl");
            write_events(
                &path,
                &[
                    serde_json::json!({"event": "tool_started", "call_id": "call-1"}),
                    serde_json::json!({"event": "tool_finished", "call_id": "call-1"}),
                    serde_json::json!({"event": terminal}),
                ],
            );
            assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
            assert!(Journal::open(temporary.path()).is_ok());
        }
    }

    #[test]
    fn empty_call_id_reuse_tracks_unfinished_work_across_terminal_runs() {
        for terminal in ["run_finished", "run_failed"] {
            for finish_new_tool in [false, true] {
                let temporary = tempfile::tempdir().unwrap();
                let path = temporary.path().join("replay-events.jsonl");
                write_events(
                    &path,
                    &[
                        serde_json::json!({"event": "tool_started", "call_id": ""}),
                        serde_json::json!({"event": "tool_finished", "call_id": ""}),
                        serde_json::json!({"event": terminal}),
                    ],
                );
                assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
                let mut owner = Journal::open(temporary.path()).unwrap();
                owner
                    .append(
                        "tool_started",
                        [("call_id".into(), Value::String("".into()))],
                    )
                    .unwrap();
                assert_eq!(Journal::find_ambiguous(&path).unwrap(), Some("".into()));
                if finish_new_tool {
                    owner
                        .append(
                            "tool_finished",
                            [("call_id".into(), Value::String("".into()))],
                        )
                        .unwrap();
                    // A tool finish alone does not prove run completion.
                    assert_eq!(Journal::find_ambiguous(&path).unwrap(), Some("".into()));
                }
                owner.append(terminal, []).unwrap();
                drop(owner);
                let committed = fs::read(&path).unwrap();
                for _ in 0..2 {
                    if finish_new_tool {
                        let reopened = Journal::open(temporary.path()).unwrap();
                        drop(reopened);
                        assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
                    } else {
                        let error = Journal::open(temporary.path()).err().unwrap();
                        assert_eq!(error.kind, ReplayErrorKind::AmbiguousExecution);
                        assert!(error.message.contains("uncertain started tool call \"\""));
                        assert_eq!(Journal::find_ambiguous(&path).unwrap(), Some("".into()));
                        // Failed admission releases its run lock, even when
                        // the diagnostic ID is empty rather than nonempty.
                        let lock = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(temporary.path().join("run.lock"))
                            .unwrap();
                        lock.try_lock_exclusive().unwrap();
                    }
                    assert_eq!(fs::read(&path).unwrap(), committed);
                }
            }
        }
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

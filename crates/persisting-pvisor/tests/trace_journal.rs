use persisting_control::trace::{Durability, Fact};
use persisting_pvisor::trace::{AppendError, Journal, Trace};
use std::io::Write;

fn event(trace: &Trace) -> persisting_control::trace::Event {
    trace.event(
        vec!["runtime:test".into()],
        None,
        None,
        vec![],
        Fact::Observation {
            domain: "vm".into(),
            name: "state.changed".into(),
            version: 1,
            payload: serde_json::json!({"state":"paused"}),
        },
    )
}

#[test]
fn durable_identity_and_idempotence_survive_reopen_and_truncated_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace.jsonl");
    let journal = Journal::open(&path).unwrap();
    assert!(Journal::open(&path).is_err());
    let trace = Trace::new(journal.clone(), "test");
    let first = event(&trace);
    let receipt = journal.append(first.clone()).unwrap();
    assert_eq!(receipt.durability, Durability::LocalSync);
    assert_eq!(journal.append(first.clone()).unwrap(), receipt);
    let mut conflict = first.clone();
    conflict.producer = "different".into();
    assert!(matches!(
        journal.append(conflict),
        Err(AppendError::Rejected(_))
    ));
    drop(trace);
    drop(journal);
    let complete_size = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"position\":")
        .unwrap();
    let torn_size = std::fs::metadata(&path).unwrap().len();
    assert!(Journal::read(&path).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), torn_size);
    let recovered = Journal::open(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), complete_size);
    assert_eq!(recovered.append(first).unwrap(), receipt);
    let trace = Trace::new(recovered.clone(), "test");
    let next = recovered.append(event(&trace)).unwrap();
    assert_eq!(next.position.journal, receipt.position.journal);
    assert_eq!(next.position.offset, 1);
    drop(trace);
    drop(recovered);
    assert_eq!(Journal::read(&path).unwrap().len(), 2);
}

#[test]
fn complete_corruption_and_old_formats_are_never_silently_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace.jsonl");
    let trace = Trace::new(Journal::open(&path).unwrap(), "test");
    trace.journal.append(event(&trace)).unwrap();
    drop(trace);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"not-json\n")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(Journal::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let old = b"{\"format\":\"pvisor.trace/2\",\"journal\":\"old\"}\n";
    std::fs::write(&path, old).unwrap();
    assert!(Journal::read(&path).is_err());
    assert!(Journal::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old);
}

#[test]
fn forward_causal_references_resolve_but_cycles_are_rejected() {
    let trace = Trace::new(Journal::memory(), "test");
    let mut a = event(&trace);
    let mut b = event(&trace);
    a.caused_by.push(b.id.clone());
    b.caused_by.push(a.id.clone());
    trace.journal.append(a.clone()).unwrap();
    assert!(matches!(
        trace.journal.append(b.clone()),
        Err(AppendError::Rejected(_))
    ));
    b.caused_by.clear();
    trace.journal.append(b).unwrap();
    assert_eq!(trace.journal.records().unwrap().len(), 2);
    a.id = "self".into();
    a.caused_by = vec!["self".into()];
    assert!(trace.journal.append(a).is_err());
}

#[tokio::test]
async fn concurrent_appends_have_unique_positions_and_duplicate_retries_converge() {
    let trace = Trace::new(Journal::memory(), "test");
    let shared = event(&trace);
    let mut jobs = Vec::new();
    for i in 0..40 {
        let journal = trace.journal.clone();
        let value = if i % 2 == 0 {
            shared.clone()
        } else {
            event(&trace)
        };
        jobs.push(tokio::spawn(async move {
            journal.append_async(value).await.unwrap()
        }));
    }
    let mut shared_position = None;
    for job in jobs {
        let receipt = job.await.unwrap();
        if receipt.event == shared.id {
            if let Some(prior) = &shared_position {
                assert_eq!(prior, &receipt.position);
            }
            shared_position = Some(receipt.position);
        }
    }
    let records = trace.journal.records().unwrap();
    assert_eq!(records.len(), 21);
    assert_eq!(
        records
            .iter()
            .map(|r| r.position.offset)
            .collect::<Vec<_>>(),
        (0..21).collect::<Vec<_>>()
    );
}

//! Snapshot output faults must not change the durable source or its retry state.
use pvisor_core::event::{Durability, Fact};
use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};
use std::io::{self, Write};

struct Output {
    bytes: Vec<u8>,
    remaining: usize,
    fault: Option<io::ErrorKind>,
    interruptions: usize,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.interruptions > 0 {
            self.interruptions -= 1;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.remaining == 0 {
            return match self.fault {
                Some(kind) => Err(kind.into()),
                None => Ok(0),
            };
        }
        // Short writes exercise continuation independently of the final fault.
        let count = bytes.len().min(self.remaining).min(7);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        panic!("snapshot must not claim to flush the caller-owned destination")
    }
}

#[test]
fn snapshot_output_faults_preserve_source_receipts_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let journal = Journal::open(&path).unwrap();
    let owner = journal.clone();
    let trace = Trace::with_id(journal.clone(), "snapshot-recovery", "contract");
    let first = trace.event(
        vec!["run".into()],
        None,
        None,
        vec![],
        Fact::Observation {
            domain: "test".into(),
            name: "snapshot-recovery".into(),
            version: 1,
            payload: serde_json::json!({"data": "small ordinary fixture"}),
        },
    );
    let receipt = journal.append(first.clone()).unwrap();
    let records = journal.records().unwrap();
    let original = std::fs::read(&path).unwrap();
    let header_len = original.iter().position(|b| *b == b'\n').unwrap() + 1;
    let mut live = owner.subscribe();

    for prefix in [0, header_len, original.len() - 1] {
        for fault in [
            None,
            Some(io::ErrorKind::Other),
            Some(io::ErrorKind::BrokenPipe),
        ] {
            let mut output = Output {
                bytes: vec![],
                remaining: prefix,
                fault,
                interruptions: 2,
            };
            let error = owner
                .snapshot_to(&mut output, original.len() as u64)
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                fault.unwrap_or(io::ErrorKind::WriteZero)
            );
            assert_eq!(output.interruptions, 0);
            assert_eq!(output.bytes, original[..prefix]);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(journal.records().unwrap(), records);
            assert_eq!(journal.append(first.clone()).unwrap(), receipt);
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));

            // Retrying into a new destination starts from the header, not the
            // source cursor left by the failed output.
            let mut recovered = Output {
                bytes: vec![],
                remaining: original.len(),
                fault: None,
                interruptions: 2,
            };
            assert_eq!(
                owner
                    .snapshot_to(&mut recovered, original.len() as u64)
                    .unwrap(),
                original.len() as u64
            );
            assert_eq!(recovered.bytes, original);
            assert_eq!(recovered.interruptions, 0);
        }
    }

    let mut second = first.clone();
    second.id = "after-output-fault".into();
    second.caused_by = vec![first.id.clone()];
    let second_receipt = owner.append(second.clone()).unwrap();
    assert_eq!(second_receipt.position.offset, receipt.position.offset + 1);
    assert_eq!(second_receipt.position.journal, receipt.position.journal);
    assert_eq!(second_receipt.durability, Durability::LocalSync);
    assert_eq!(live.try_recv().unwrap(), second);
    assert!(matches!(
        live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let final_records = journal.records().unwrap();
    drop(trace);
    drop(owner);
    drop(journal);
    Journal::validate(&path).unwrap();
    assert_eq!(Journal::read(&path).unwrap(), final_records);
    let reopened = Journal::open(&path).unwrap();
    assert_eq!(reopened.append(first).unwrap(), receipt);
    assert_eq!(reopened.append(second).unwrap(), second_receipt);
    assert_eq!(reopened.records().unwrap(), final_records);
}

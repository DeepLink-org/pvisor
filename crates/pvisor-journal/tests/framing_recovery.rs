//! Seeded byte-framing faults at reader-buffer and maximum-line boundaries.
use pvisor_core::event::{Durability, Event, Fact, MAX_EVENT_BYTES};
use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn seeded_framing_limits_reject_complete_damage_and_recover_bounded_tails() {
    let first = std::env::var("PVISOR_CHAOS_FIRST_SEED")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(202611120001);
    let count = std::env::var("PVISOR_CHAOS_SEED_COUNT")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(16);
    assert!((1..=4096).contains(&count));
    let limit = MAX_EVENT_BYTES + 4096;
    let mut repaired = 0;
    let mut oversized = 0;
    let mut complete_rejected = 0;
    for seed in first..first.checked_add(count).unwrap() {
        // Keep failure evidence; successful fixtures are explicitly removed.
        let root = tempfile::Builder::new()
            .prefix("frame-")
            .tempdir()
            .unwrap()
            .keep();
        eprintln!("framing seed={seed} fixture={}", root.display());
        let path = root.join("trace");
        let journal = Journal::open(&path).unwrap();
        let trace = Trace::with_id(journal.clone(), format!("trace-{seed}"), "framing");
        let mut random = seed | 1;
        let mut events: Vec<Event> = Vec::new();
        let mut receipts = Vec::new();
        for index in 0..3 {
            let mut event = trace.event(
                vec!["chaos".into()],
                None,
                None,
                events
                    .last()
                    .map(|e| vec![e.id.clone()])
                    .unwrap_or_default(),
                Fact::Observation {
                    domain: "journal-framing".into(),
                    name: "prefix".into(),
                    version: 1,
                    payload: serde_json::json!({
                        "seed":seed,
                        "index":index,
                        "text":"零\n\0\\\"".repeat((next(&mut random) % 1024 + 1) as usize)
                    }),
                },
            );
            event.id = format!("event-{seed}-{index}");
            event.observed_at_unix_ms = 1;
            receipts.push(journal.append(event.clone()).unwrap());
            events.push(event);
        }
        let records = journal.records().unwrap();
        assert_eq!(records.len(), events.len());
        for (index, ((record, event), receipt)) in
            records.iter().zip(&events).zip(&receipts).enumerate()
        {
            assert_eq!(&record.event, event);
            assert_eq!(record.position, receipt.position);
            assert_eq!(record.position.offset, index as u64);
            assert_eq!(record.position.journal, receipts[0].position.journal);
        }
        drop(trace);
        drop(journal);
        let good = fs::read(&path).unwrap();
        let serialized_record = serde_json::to_vec(&records[2]).unwrap();
        let sizes = [0, 1, 7, 8191, 8192, 8193, limit - 1, limit, limit + 1];
        for kind in 0..12 {
            let mut damaged = good.clone();
            let reject_complete = kind == 11;
            let reject_size = kind == 8;
            if kind < sizes.len() {
                // No newline anywhere: invalid UTF-8 still represents an
                // incomplete tail, not a corrupt complete frame.
                let pattern = [0x80, 0xff, b'{', b'"', b'\\', b' '];
                let byte = pattern[(next(&mut random) % pattern.len() as u64) as usize];
                damaged.extend(std::iter::repeat_n(byte, sizes[kind]));
            } else if kind == 9 {
                // Even a complete JSON value is uncommitted framing without LF.
                damaged.extend_from_slice(&serialized_record);
            } else if kind == 10 {
                let cut = 1 + next(&mut random) as usize % (serialized_record.len() - 1);
                damaged.extend_from_slice(&serialized_record[..cut]);
            } else {
                // A complete invalid frame must prevent all repair, including
                // truncating a later otherwise-repairable tail.
                match seed % 4 {
                    0 => damaged.extend_from_slice(b"\xff\n"),
                    1 => damaged.extend_from_slice(b"{}\n"),
                    2 => {
                        let mut invalid = serde_json::to_value(&records[0]).unwrap();
                        invalid["position"]["offset"] = 3.into();
                        // Reuses a committed event identity at a new offset.
                        damaged.extend(serde_json::to_vec(&invalid).unwrap());
                        damaged.push(b'\n');
                    }
                    _ => {
                        let mut invalid = serde_json::to_value(&records[0]).unwrap();
                        invalid["position"]["offset"] = 3.into();
                        invalid["event"]["id"] = format!("invalid-{seed}").into();
                        invalid["event"]["scope"] = serde_json::json!([]);
                        damaged.extend(serde_json::to_vec(&invalid).unwrap());
                        damaged.push(b'\n');
                    }
                }
                damaged.extend(std::iter::repeat_n(
                    0xfe,
                    1 + (next(&mut random) % 8193) as usize,
                ));
            }
            fs::write(&path, &damaged).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            if kind == 0 {
                assert_eq!(Journal::read(&path).unwrap(), records);
                Journal::validate(&path).unwrap();
            } else {
                assert!(Journal::read(&path).is_err(), "seed={seed} kind={kind}");
                assert!(Journal::validate(&path).is_err(), "seed={seed} kind={kind}");
            }
            assert_eq!(fs::read(&path).unwrap(), damaged);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
            if reject_complete || reject_size {
                assert!(Journal::open(&path).is_err(), "seed={seed} kind={kind}");
                assert_eq!(fs::read(&path).unwrap(), damaged);
                if reject_complete {
                    complete_rejected += 1;
                } else {
                    oversized += 1;
                }
                // Explicit fixture repair, not product recovery of corruption.
                fs::write(&path, &good).unwrap();
            } else if kind != 0 {
                repaired += 1;
            }
            let recovered = Journal::open(&path).unwrap();
            assert_eq!(fs::read(&path).unwrap(), good, "seed={seed} kind={kind}");
            assert_eq!(recovered.records().unwrap(), records);
            let mut live = recovered.subscribe();
            for (event, receipt) in events.iter().zip(&receipts) {
                assert_eq!(recovered.append(event.clone()).unwrap(), *receipt);
                assert_eq!(receipt.durability, Durability::LocalSync);
            }
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
            let mut healthy = events[2].clone();
            healthy.id = format!("healthy-{seed}-{kind}");
            let receipt = recovered.append(healthy.clone()).unwrap();
            assert_eq!(receipt.position.offset, 3);
            assert_eq!(receipt.position.journal, receipts[0].position.journal);
            assert_eq!(receipt.durability, Durability::LocalSync);
            assert_eq!(live.try_recv().unwrap(), healthy);
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
            let keeper = recovered.clone();
            drop(recovered);
            assert!(Journal::open(&path).is_err());
            assert!(Journal::read(&path).is_err());
            assert!(Journal::validate(&path).is_err());
            drop(keeper);
            Journal::validate(&path).unwrap();
            let after = Journal::read(&path).unwrap();
            assert_eq!(after.len(), 4);
            assert_eq!(&after[..3], records.as_slice());
            assert_eq!(after[3].event, healthy);
            let reopened = Journal::open(&path).unwrap();
            let mut live = reopened.subscribe();
            assert_eq!(reopened.append(healthy).unwrap(), receipt);
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
            assert_eq!(reopened.records().unwrap(), after);
            drop(reopened);
        }
        fs::remove_dir_all(&root).unwrap();
    }
    eprintln!(
        "framing seeds={count} cases={} repaired={repaired} oversized_rejected={oversized} complete_rejected={complete_rejected} healthy_appends={} reopen_checks={} failure_fixtures=0",
        count * 12,
        count * 12,
        count * 24
    );
}

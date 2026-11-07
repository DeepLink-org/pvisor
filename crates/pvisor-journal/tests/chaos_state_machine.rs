//! Bounded, reproducible fault sequences through the public Journal API.
use pvisor_core::event::{Event, Fact, Receipt};
use pvisor_journal::api::{AppendError, Journal, JournalStore, Trace, TraceProducer};

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

// Independent of the implementation's DFS: repeatedly remove nodes whose causes
// are outside the remaining graph. A nonempty fixed point contains a cycle.
fn acyclic(events: &[Event]) -> bool {
    let mut remaining: Vec<_> = events.iter().collect();
    while !remaining.is_empty() {
        let ids: Vec<_> = remaining.iter().map(|e| e.id.clone()).collect();
        let before = remaining.len();
        remaining.retain(|e| e.caused_by.iter().any(|cause| ids.contains(cause)));
        if remaining.len() == before {
            return false;
        }
    }
    true
}

#[test]
fn seeded_retries_conflicts_cycles_and_reopens_match_a_graph_model() {
    let root = tempfile::tempdir().unwrap();
    let mut new_commits = 0;
    let mut retries = 0;
    let mut conflicts = 0;
    let mut cycles = 0;
    for seed in 1..=16 {
        let mut random = seed;
        let path = root.path().join(format!("trace-{seed}"));
        let memory = Journal::memory();
        let mut durable = Journal::open(&path).unwrap();
        let trace = Trace::with_id(memory.clone(), format!("seed-{seed}"), "chaos");
        let templates: Vec<_> = (0..24)
            .map(|index| {
                let mut e = trace.event(
                    vec!["test".into()],
                    None,
                    None,
                    vec![],
                    Fact::Observation {
                        domain: "test".into(),
                        name: "state-machine".into(),
                        version: 1,
                        payload: serde_json::Value::Null,
                    },
                );
                e.id = format!("node-{index}");
                e.observed_at_unix_ms = 1;
                for _ in 0..next(&mut random) % 4 {
                    let cause = format!("node-{}", next(&mut random) % 24);
                    if cause != e.id && !e.caused_by.contains(&cause) {
                        e.caused_by.push(cause);
                    }
                }
                e
            })
            .collect();
        let mut committed: Vec<Event> = Vec::new();
        let mut receipts: Vec<Receipt> = Vec::new();
        for step in 0..128 {
            let mut e = templates[(next(&mut random) % 24) as usize].clone();
            if next(&mut random).is_multiple_of(5) {
                e.observed_at_unix_ms += 1;
            }
            let prior = committed.iter().position(|old| old.id == e.id);
            let expected = if let Some(index) = prior {
                if committed[index] == e {
                    retries += 1;
                    Some(index)
                } else {
                    conflicts += 1;
                    None
                }
            } else {
                let mut proposed = committed.clone();
                proposed.push(e.clone());
                if acyclic(&proposed) {
                    Some(committed.len())
                } else {
                    cycles += 1;
                    None
                }
            };
            for (journal, is_durable) in [(&memory, false), (&durable, true)] {
                let mut live = journal.subscribe();
                let result = journal.append(e.clone());
                match expected {
                    Some(index) => {
                        let receipt = result
                            .unwrap_or_else(|error| panic!("seed={seed} step={step}: {error}"));
                        assert_eq!(receipt.position.offset, index as u64);
                        if prior.is_some() {
                            assert!(live.try_recv().is_err());
                            if is_durable {
                                assert_eq!(receipt, receipts[index]);
                            }
                        } else {
                            assert_eq!(live.try_recv().unwrap(), e);
                            if is_durable {
                                receipts.push(receipt);
                            }
                        }
                    }
                    None => {
                        assert!(
                            matches!(result, Err(AppendError::Rejected(_))),
                            "seed={seed} step={step}: {result:?}"
                        );
                        assert!(live.try_recv().is_err());
                    }
                }
            }
            if expected == Some(committed.len()) {
                committed.push(e);
                new_commits += 1;
            }
            if step % 16 == 15 {
                drop(durable);
                Journal::validate(&path).unwrap();
                let read = Journal::read(&path).unwrap();
                assert_eq!(
                    read.into_iter().map(|r| r.event).collect::<Vec<_>>(),
                    committed,
                    "seed={seed} step={step}"
                );
                durable = Journal::open(&path).unwrap();
                for journal in [&memory, &durable] {
                    assert_eq!(
                        journal
                            .records()
                            .unwrap()
                            .into_iter()
                            .map(|r| r.event)
                            .collect::<Vec<_>>(),
                        committed,
                        "seed={seed} step={step}"
                    );
                }
            }
        }
    }
    assert!(new_commits > 0 && retries > 0 && conflicts > 0 && cycles > 0);
    eprintln!(
        "2048 transitions: {new_commits} commits, {retries} retries, \
         {conflicts} conflicts, {cycles} cycle rejections; 128 reopen checks"
    );
}

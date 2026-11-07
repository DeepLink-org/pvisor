//! External contracts: no implementation state is accessible to these tests.
use pvisor_core::event::{Durability, Event, Fact, Granularity, Level, MAX_EVENT_BYTES, VERSION};
use pvisor_journal::api::{
    AppendError, DurableFiles, Journal, JournalStore, Persistence, Trace, TraceProducer,
};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
};
use syn::visit::{self, Visit};

fn fact() -> Fact {
    Fact::Observation {
        domain: "test".into(),
        name: "step".into(),
        version: 1,
        payload: serde_json::json!({"done":true}),
    }
}
fn event(journal: &Journal) -> Event {
    Trace::new(journal.clone(), "contract").event(vec!["run".into()], None, None, vec![], fact())
}
fn send<T: Send>(_: T) {}

#[test]
fn floating_payload_retries_keep_their_receipt_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let mut event = event(&journal);
    event.data = Fact::Observation {
        domain: "test".into(),
        name: "float".into(),
        version: 1,
        payload: serde_json::json!({"values": [
            -1.2385131597440577e-185_f64, f64::MIN_POSITIVE,
            f64::MAX, f64::from_bits(1), -0.0_f64
        ]}),
    };
    let receipt = journal.append(event.clone()).unwrap();
    drop(journal);
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.records().unwrap()[0].event, event);
    assert_eq!(journal.append(event.clone()).unwrap(), receipt);
    assert_eq!(journal.records().unwrap().len(), 1);
    if let Fact::Observation { payload, .. } = &mut event.data {
        payload["values"][0] = serde_json::json!(1.0);
    }
    assert!(matches!(
        journal.append(event),
        Err(AppendError::Rejected(_))
    ));
}

#[test]
fn deeply_nested_payload_is_rejected_before_commit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    for journal in [Journal::memory(), Journal::open(&path).unwrap()] {
        let mut live = journal.subscribe();
        let mut before = Vec::new();
        let durable = journal.snapshot_to(&mut before, u64::MAX).is_ok();
        let mut nested = event(&journal);
        let mut payload = serde_json::Value::Null;
        for _ in 0..160 {
            payload = serde_json::json!([payload]);
        }
        nested.data = Fact::Observation {
            domain: "test".into(),
            name: "nested".into(),
            version: 1,
            payload,
        };
        assert!(matches!(
            journal.append(nested.clone()),
            Err(AppendError::Rejected(_))
        ));
        assert!(live.try_recv().is_err());
        assert!(journal.records().unwrap().is_empty());
        if durable {
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        if let Fact::Observation { payload, .. } = &mut nested.data {
            *payload = serde_json::Value::Null;
            for _ in 0..32 {
                *payload = serde_json::json!([payload.take()]);
            }
        }
        assert_eq!(journal.append(nested).unwrap().position.offset, 0);
    }
    let journal = Journal::open(&path).unwrap();
    assert_eq!(journal.records().unwrap().len(), 1);
}

#[test]
fn interrupted_record_writes_recover_only_complete_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let mut events = Vec::new();
    let mut receipts = Vec::new();
    for _ in 0..3 {
        let e = event(&journal);
        receipts.push(journal.append(e.clone()).unwrap());
        events.push(e);
    }
    drop(journal);
    let good = std::fs::read(&path).unwrap();
    let boundaries: Vec<_> = good
        .iter()
        .enumerate()
        .filter_map(|(i, b)| (*b == b'\n').then_some(i + 1))
        .collect();
    let header_end = boundaries[0];
    let mut cuts = boundaries.clone();
    for boundary in &boundaries {
        cuts.push(boundary - 1);
    }
    // Deterministic fault locations across headers and all three records.
    for i in 1..=64 {
        cuts.push(1 + i * (good.len() - 1) / 64);
    }
    cuts.sort_unstable();
    cuts.dedup();
    for cut in cuts {
        std::fs::write(&path, &good[..cut]).unwrap();
        if cut < header_end {
            assert!(Journal::open(&path).is_err(), "header cut at {cut}");
            assert_eq!(std::fs::read(&path).unwrap(), good[..cut]);
            continue;
        }
        let complete_end = *boundaries.iter().rfind(|b| **b <= cut).unwrap();
        let committed = boundaries.iter().filter(|b| **b <= cut).count() - 1;
        if cut != complete_end {
            assert!(Journal::read(&path).is_err(), "tail cut at {cut}");
            assert!(Journal::validate(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), good[..cut]);
        }
        let recovered = Journal::open(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), good[..complete_end]);
        assert_eq!(recovered.records().unwrap().len(), committed);
        for (e, receipt) in events.iter().zip(&receipts) {
            assert_eq!(recovered.append(e.clone()).unwrap(), *receipt);
        }
        drop(recovered);
        assert_eq!(Journal::read(&path).unwrap().len(), 3);
        Journal::validate(&path).unwrap();
    }
}

#[test]
fn api_is_unconditional_declarations_and_only_public_root() {
    struct Guard;
    impl<'ast> Visit<'ast> for Guard {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(!attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"));
            visit::visit_attribute(self, attr);
        }
        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("API implementation");
        }
        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("API function body");
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            assert!(item.default.is_none());
            visit::visit_trait_item_fn(self, item);
        }
        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
            panic!("nested API module");
        }
        fn visit_item_macro(&mut self, _: &'ast syn::ItemMacro) {
            panic!("hidden API declarations");
        }
    }
    Guard.visit_file(&syn::parse_file(include_str!("../src/api.rs")).unwrap());
    let root = syn::parse_file(include_str!("../src/lib.rs")).unwrap();
    assert!(root.attrs.iter().any(|attr| attr.path().is_ident("deny")
        && attr.meta.require_list().unwrap().tokens.to_string() == "missing_docs"));
    let mut public = vec![];
    for item in root.items {
        let syn::Item::Mod(module) = item else {
            panic!("root compatibility export");
        };
        assert!(module.content.is_none());
        if matches!(module.vis, syn::Visibility::Public(_)) {
            assert!(module.attrs.is_empty());
            public.push(module.ident.to_string());
        }
    }
    assert_eq!(public, ["api"]);
}

#[test]
fn recursive_private_source_guard() {
    struct Guard;
    impl<'ast> Visit<'ast> for Guard {
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if item.trait_.is_none() {
                for member in &item.items {
                    if let syn::ImplItem::Fn(method) = member {
                        assert!(
                            !matches!(method.vis, syn::Visibility::Public(_)),
                            "undeclared public inherent method {}",
                            method.sig.ident
                        );
                    }
                }
            }
            visit::visit_item_impl(self, item);
        }
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            assert!(
                !matches!(item.vis, syn::Visibility::Public(_)),
                "public implementation module"
            );
            visit::visit_item_mod(self, item);
        }
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            assert!(
                !matches!(item.vis, syn::Visibility::Public(_)),
                "public standalone implementation helper"
            );
            visit::visit_item_fn(self, item);
        }
        fn visit_field(&mut self, field: &'ast syn::Field) {
            assert!(
                matches!(field.vis, syn::Visibility::Inherited),
                "opaque state must stay private"
            );
            visit::visit_field(self, field);
        }
    }
    fn scan(path: &Path, root: &Path, count: &mut usize) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, root, count);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path != root.join("lib.rs")
                && path != root.join("api.rs")
            {
                Guard
                    .visit_file(&syn::parse_file(&std::fs::read_to_string(path).unwrap()).unwrap());
                *count += 1;
            }
        }
    }
    let mut count = 0;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    scan(&root, &root, &mut count);
    assert!(count >= 3);
}

#[test]
fn owners_signatures_defaults_and_send_contracts() {
    fn storage<T: JournalStore + Clone + Default + Send + Sync>() {}
    fn producer<T: TraceProducer + Clone + Send + Sync>() {}
    fn files<T: DurableFiles>() {}
    storage::<Journal>();
    producer::<Trace>();
    files::<Persistence>();
    let _: fn(&Path) -> anyhow::Result<Journal> = Journal::open;
    let _: fn(&Path) -> anyhow::Result<Vec<pvisor_core::event::Record>> = Journal::read;
    let _: fn(&Path) -> anyhow::Result<()> = Journal::validate;
    let _: fn(&Journal, Event) -> Result<pvisor_core::event::Receipt, AppendError> =
        Journal::append;
    let _: fn(&Trace) -> &str = Trace::id;
    let _: fn(&Trace) -> &str = Trace::producer;
    let _: fn(&Trace) -> &Journal = Trace::journal;
    let journal = Journal::default();
    let independent = Journal::memory();
    assert!(journal.records().unwrap().is_empty());
    assert_ne!(
        journal.append(event(&journal)).unwrap().position.journal,
        independent
            .append(event(&independent))
            .unwrap()
            .position
            .journal
    );
    send(journal.append_async(event(&journal)));
    let trace = Trace::new(journal, "send");
    send(trace.emit(event(trace.journal())));
    assert!(std::error::Error::source(&AppendError::Rejected("test".into())).is_none());
    assert!(
        AppendError::Unknown("test".into())
            .to_string()
            .contains("reopen")
    );
}

#[test]
fn receipts_idempotency_and_live_commit_memory_and_durable() {
    let root = tempfile::tempdir().unwrap();
    for journal in [
        Journal::memory(),
        Journal::open(&root.path().join("trace")).unwrap(),
    ] {
        let durable = journal.snapshot_to(&mut Vec::new(), u64::MAX).is_ok();
        let mut live = journal.subscribe();
        let original = event(&journal);
        let receipt = journal.append(original.clone()).unwrap();
        assert_eq!(receipt.event, original.id);
        assert_eq!(receipt.position.offset, 0);
        assert_eq!(
            receipt.durability,
            if durable {
                Durability::LocalSync
            } else {
                Durability::Volatile
            }
        );
        assert_eq!(live.try_recv().unwrap(), original);
        assert_eq!(journal.records().unwrap()[0].event, original);
        assert_eq!(journal.clone().append(original.clone()).unwrap(), receipt);
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let mut changed = original.clone();
        changed.observed_at_unix_ms += 1;
        assert!(matches!(
            journal.append(changed),
            Err(AppendError::Rejected(_))
        ));
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let second = journal.append(event(&journal)).unwrap();
        assert_eq!(second.position.offset, 1);
        assert_eq!(second.position.journal, receipt.position.journal);
        let mut copy = journal.records().unwrap();
        copy.clear();
        assert_eq!(journal.records().unwrap().len(), 2);
    }
}

#[test]
fn invalid_events_and_causal_cycles_do_not_change_storage() {
    let root = tempfile::tempdir().unwrap();
    for journal in [
        Journal::memory(),
        Journal::open(&root.path().join("trace")).unwrap(),
    ] {
        let mut live = journal.subscribe();
        let original = event(&journal);
        let mut invalid = vec![];
        let mut e = original.clone();
        e.version = VERSION - 1;
        invalid.push(e);
        let mut e = original.clone();
        e.id = " ".into();
        invalid.push(e);
        let mut e = original.clone();
        e.trace_id = "x".repeat(257);
        invalid.push(e);
        let mut e = original.clone();
        e.producer.clear();
        invalid.push(e);
        let mut e = original.clone();
        e.scope.clear();
        invalid.push(e);
        let mut e = original.clone();
        e.scope = vec!["s".into(); 17];
        invalid.push(e);
        let mut e = original.clone();
        e.context = Some(String::new());
        invalid.push(e);
        let mut e = original.clone();
        e.caused_by = vec![e.id.clone()];
        invalid.push(e);
        let mut e = original.clone();
        e.caused_by = vec!["cause".into(); 2];
        invalid.push(e);
        let mut e = original.clone();
        e.caused_by = (0..65).map(|i| i.to_string()).collect();
        invalid.push(e);
        let mut e = original.clone();
        e.data = Fact::Observation {
            domain: "test".into(),
            name: "step".into(),
            version: 0,
            payload: serde_json::Value::Null,
        };
        invalid.push(e);
        let mut e = original.clone();
        e.data = Fact::Observation {
            domain: "test".into(),
            name: "step".into(),
            version: 1,
            payload: serde_json::Value::String("x".repeat(MAX_EVENT_BYTES)),
        };
        invalid.push(e);
        for e in invalid {
            assert!(matches!(journal.append(e), Err(AppendError::Rejected(_))));
        }
        assert!(journal.records().unwrap().is_empty());
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let mut a = original;
        a.id = "a".into();
        a.caused_by = vec!["b".into()];
        journal.append(a).unwrap();
        let mut b = event(&journal);
        b.id = "b".into();
        b.caused_by = vec!["a".into()];
        assert!(matches!(
            journal.append(b.clone()),
            Err(AppendError::Rejected(_))
        ));
        assert_eq!(journal.records().unwrap().len(), 1);
        b.caused_by = vec!["unresolved".into()];
        assert_eq!(journal.append(b).unwrap().position.offset, 1);
    }
}

#[test]
fn exclusive_writer_lock_survives_clones_and_producer_drop() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let copy = journal.clone();
    let trace = Trace::new(copy.clone(), "lock");
    drop(journal);
    assert!(Journal::open(&path).is_err());
    assert!(Journal::read(&path).is_err());
    assert!(Journal::validate(&path).is_err());
    drop(copy);
    assert!(Journal::open(&path).is_err());
    assert!(trace.journal().records().unwrap().is_empty());
    drop(trace);
    assert!(Journal::read(&path).unwrap().is_empty());
    Journal::validate(&path).unwrap();
    drop(Journal::open(&path).unwrap());
}

#[test]
fn restart_reuses_receipt_and_repairs_only_incomplete_tail() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let e = event(&journal);
    let receipt = journal.append(e.clone()).unwrap();
    drop(journal);
    let good = std::fs::read(&path).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{incomplete")
        .unwrap();
    let incomplete = std::fs::read(&path).unwrap();
    assert!(Journal::read(&path).is_err());
    assert!(Journal::validate(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), incomplete);
    let reopened = Journal::open(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), good);
    let mut live = reopened.subscribe();
    assert_eq!(reopened.append(e).unwrap(), receipt);
    assert!(live.try_recv().is_err());
    assert_eq!(
        reopened.append(event(&reopened)).unwrap().position.offset,
        1
    );
    drop(reopened);
    assert_eq!(Journal::read(&path).unwrap().len(), 2);
    for bytes in [b"{invalid}\n".to_vec(), b"{incomplete header".to_vec()] {
        let corrupt = root.path().join("corrupt");
        std::fs::write(&corrupt, &bytes).unwrap();
        assert!(Journal::open(&corrupt).is_err());
        assert_eq!(std::fs::read(&corrupt).unwrap(), bytes);
    }
    let mut corrupt = good.clone();
    corrupt.extend_from_slice(b"{invalid}\n");
    std::fs::write(&path, &corrupt).unwrap();
    assert!(Journal::open(&path).is_err());
    assert!(Journal::validate(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
}

#[test]
fn restart_preserves_float_content_and_the_original_retry_receipt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let mut e = event(&journal);
    e.data = Fact::Observation {
        domain: "test".into(),
        name: "numeric".into(),
        version: 1,
        payload: serde_json::json!([
            -1.2385131597440577e-185_f64,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::from_bits(1),
            -0.0_f64,
        ]),
    };
    let receipt = journal.append(e.clone()).unwrap();
    drop(journal);
    let reopened = Journal::open(&path).unwrap();
    assert_eq!(reopened.records().unwrap()[0].event, e);
    assert_eq!(reopened.append(e).unwrap(), receipt);
    assert_eq!(reopened.records().unwrap().len(), 1);
}

#[test]
fn corrupt_complete_graph_before_torn_tail_is_rejected_without_truncation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    let first = event(&journal);
    let mut forward = event(&journal);
    forward.caused_by = vec!["future".into()];
    let receipts = [
        journal.append(first.clone()).unwrap(),
        journal.append(forward.clone()).unwrap(),
    ];
    let prefix = journal.records().unwrap();
    let mut future = event(&journal);
    future.id = "future".into();
    future.caused_by = vec![forward.id.clone()];
    drop(journal);
    let good = std::fs::read(&path).unwrap();

    // File-state injection, not a process crash: a complete record closes the
    // previously unresolved edge into a cycle, followed by an incomplete write.
    // open may discard only the incomplete tail, never a corrupt complete graph.
    let mut cyclic = serde_json::to_value(&prefix[1]).unwrap();
    cyclic["position"]["offset"] = 2.into();
    cyclic["event"] = serde_json::to_value(&future).unwrap();
    let mut damaged = good.clone();
    damaged.extend(serde_json::to_vec(&cyclic).unwrap());
    damaged.extend_from_slice(b"\n{\"position\":");
    std::fs::write(&path, &damaged).unwrap();
    assert!(Journal::read(&path).is_err());
    assert!(Journal::validate(&path).is_err());
    assert!(Journal::open(&path).is_err());
    // Even a failed writer open must not truncate the incomplete tail before
    // validating the complete prefix's causal graph.
    assert_eq!(std::fs::read(&path).unwrap(), damaged);

    // Explicit fixture repair removes only the injected complete bad record;
    // this is not automatic recovery of corruption by the product.
    let mut torn = good.clone();
    torn.extend_from_slice(b"{\"position\":");
    std::fs::write(&path, &torn).unwrap();
    assert!(Journal::read(&path).is_err());
    assert!(Journal::validate(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), torn);
    let recovered = Journal::open(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), good);
    assert_eq!(recovered.records().unwrap(), prefix);
    let mut live = recovered.subscribe();
    assert_eq!(recovered.append(first).unwrap(), receipts[0]);
    assert_eq!(recovered.append(forward).unwrap(), receipts[1]);
    assert!(live.try_recv().is_err());
    // Recovery must rebuild unresolved forward references, not merely offsets.
    assert!(matches!(
        recovered.append(future.clone()),
        Err(AppendError::Rejected(_))
    ));
    assert_eq!(recovered.records().unwrap(), prefix);
    assert_eq!(std::fs::read(&path).unwrap(), good);
    assert!(live.try_recv().is_err());
    future.caused_by = vec![prefix[0].event.id.clone()];
    let receipt = recovered.append(future.clone()).unwrap();
    assert_eq!(receipt.position.offset, 2);
    assert_eq!(receipt.position.journal, receipts[0].position.journal);
    assert_eq!(receipt.durability, Durability::LocalSync);
    assert_eq!(live.try_recv().unwrap(), future);
    let records = recovered.records().unwrap();
    assert_eq!(&records[..2], prefix.as_slice());
    assert_eq!(records[2].event, future);
    assert_eq!(records[2].position, receipt.position);
    drop(recovered);
    Journal::validate(&path).unwrap();
    assert_eq!(Journal::read(&path).unwrap(), records);
    let reopened = Journal::open(&path).unwrap();
    assert_eq!(reopened.append(future).unwrap(), receipt);
    assert_eq!(reopened.records().unwrap(), records);
}

#[test]
fn complete_semantic_corruption_is_not_repaired() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    journal.append(event(&journal)).unwrap();
    drop(journal);
    let bytes = std::fs::read_to_string(&path).unwrap();
    let mut lines = bytes.lines();
    let header = lines.next().unwrap();
    let record: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut corruptions = vec![];
    let mut shifted = record.clone();
    shifted["position"]["offset"] = 1.into();
    corruptions.push(format!("{header}\n{shifted}\n"));
    let mut wrong_journal = record.clone();
    wrong_journal["position"]["journal"] = "different".into();
    corruptions.push(format!("{header}\n{wrong_journal}\n"));
    let mut invalid_event = record.clone();
    invalid_event["event"]["scope"] = serde_json::json!([]);
    corruptions.push(format!("{header}\n{invalid_event}\n"));
    let mut duplicate = record.clone();
    duplicate["position"]["offset"] = 1.into();
    corruptions.push(format!("{header}\n{record}\n{duplicate}\n"));
    let mut a = record.clone();
    a["event"]["id"] = "a".into();
    a["event"]["caused_by"] = serde_json::json!(["b"]);
    let mut b = record;
    b["position"]["offset"] = 1.into();
    b["event"]["id"] = "b".into();
    b["event"]["caused_by"] = serde_json::json!(["a"]);
    corruptions.push(format!("{header}\n{a}\n{b}\n"));
    corruptions.push(bytes.replace(&format!("pvisor.trace/{VERSION}"), "pvisor.trace/0"));
    corruptions.push(format!("{header}\n{}", "x".repeat(MAX_EVENT_BYTES + 4097)));
    for bad in corruptions {
        std::fs::write(&path, &bad).unwrap();
        assert!(Journal::read(&path).is_err());
        assert!(Journal::validate(&path).is_err());
        assert!(Journal::open(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
    }
}

#[test]
fn snapshot_bounds_and_output_failure_leave_source_usable() {
    struct Fails {
        bytes: Vec<u8>,
    }
    impl Write for Fails {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes.is_empty() {
                self.bytes.extend_from_slice(&bytes[..3]);
                Ok(3)
            } else {
                Err(io::Error::other("injected output failure"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(
        Journal::memory()
            .snapshot_to(&mut vec![], u64::MAX)
            .is_err()
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let journal = Journal::open(&path).unwrap();
    journal.append(event(&journal)).unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    let mut output = vec![];
    assert!(journal.snapshot_to(&mut output, size - 1).is_err());
    assert!(output.is_empty());
    let mut fails = Fails { bytes: vec![] };
    assert!(journal.snapshot_to(&mut fails, size).is_err());
    assert_eq!(fails.bytes.len(), 3);
    assert_eq!(journal.snapshot_to(&mut output, size).unwrap(), size);
    assert_eq!(output, std::fs::read(&path).unwrap());
    assert_eq!(journal.append(event(&journal)).unwrap().position.offset, 1);
    assert_eq!(journal.records().unwrap().len(), 2);
    let snapshot = root.path().join("snapshot");
    std::fs::write(&snapshot, output).unwrap();
    assert_eq!(Journal::read(&snapshot).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_concurrent_retry_is_one_commit_and_notifications_can_lag() {
    let root = tempfile::tempdir().unwrap();
    for journal in [
        Journal::memory(),
        Journal::open(&root.path().join("trace")).unwrap(),
    ] {
        let mut live = journal.subscribe();
        let e = event(&journal);
        let mut tasks = vec![];
        for _ in 0..16 {
            let j = journal.clone();
            let e = e.clone();
            tasks.push(tokio::spawn(
                async move { j.append_async(e).await.unwrap() },
            ));
        }
        let mut receipts = vec![];
        for task in tasks {
            receipts.push(task.await.unwrap());
        }
        assert!(receipts.iter().all(|r| r == &receipts[0]));
        assert_eq!(live.try_recv().unwrap(), e);
        assert!(live.try_recv().is_err());
        let mut distinct = vec![];
        for _ in 0..16 {
            let j = journal.clone();
            let e = event(&j);
            distinct.push(tokio::spawn(async move {
                j.append_async(e).await.unwrap().position.offset
            }));
        }
        let mut offsets = vec![];
        for task in distinct {
            offsets.push(task.await.unwrap());
        }
        offsets.sort();
        assert_eq!(offsets, (1..=16).collect::<Vec<_>>());
        for _ in 0..300 {
            journal.append(event(&journal)).unwrap();
        }
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
        ));
        assert_eq!(journal.records().unwrap().len(), 317);
    }
    let journal = Journal::memory();
    let mut receiver = journal.subscribe();
    journal.append(event(&journal)).unwrap();
    drop(journal);
    receiver.recv().await.unwrap();
    assert!(matches!(
        receiver.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn explicit_trace_identity_readonly_accessors_and_emit_semantics() {
    let journal = Journal::memory();
    let trace = Trace::with_id(
        journal.clone(),
        " supplied identity ".to_string(),
        "producer".to_string(),
    );
    let clone = trace.clone();
    assert_eq!(clone.id(), " supplied identity ");
    assert_eq!(clone.producer(), "producer");
    let e = clone.event(
        vec!["scope".into()],
        Some("context".into()),
        Some("operation".into()),
        vec!["cause".into()],
        fact(),
    );
    assert_eq!(e.trace_id, trace.id());
    assert_eq!(e.producer, trace.producer());
    assert_eq!(e.context.as_deref(), Some("context"));
    assert_eq!(e.operation.as_deref(), Some("operation"));
    assert_eq!(e.caused_by, ["cause"]);
    assert_eq!(e.level, Level::Info);
    assert_eq!(e.granularity, Granularity::Operation);
    assert_ne!(
        e.id,
        clone
            .event(vec!["scope".into()], None, None, vec![], fact())
            .id
    );
    let other = event(&journal);
    assert_ne!(other.trace_id, trace.id());
    assert_eq!(trace.emit(other.clone()).await.unwrap(), other.id);
    assert_eq!(clone.journal().records().unwrap()[0].event, other);
    let bad = Trace::with_id(journal, "", "producer");
    let error = bad
        .emit(bad.event(vec!["scope".into()], None, None, vec![], fact()))
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<AppendError>(),
        Some(AppendError::Rejected(_))
    ));
    let fresh = Trace::new(Journal::memory(), "new");
    assert!(uuid::Uuid::parse_str(fresh.id()).is_ok());
}

#[test]
fn trace_event_projection_preserves_granularity_levels_and_inputs() {
    use pvisor_core::{
        event::Origin,
        operation::{Context, Failure, Outcome},
    };
    let trace = Trace::with_id(Journal::memory(), "trace", "producer");
    let scope = vec!["scope".into()];
    let context = trace.event(
        scope.clone(),
        Some("context".into()),
        None,
        vec![],
        Fact::Context {
            definition: Context {
                revision: 1,
                principal: "principal".into(),
                scope: scope.clone(),
                policy: "policy".into(),
                bindings: Default::default(),
            },
        },
    );
    assert_eq!(context.granularity, Granularity::Detail);
    assert_eq!(context.level, Level::Info);
    for (failure, level) in [
        (
            Failure::Denied {
                reason: "denied".into(),
            },
            Level::Warn,
        ),
        (
            Failure::Unsupported {
                reason: "unsupported".into(),
            },
            Level::Warn,
        ),
        (
            Failure::Failed {
                domain: "test".into(),
                code: "failed".into(),
                effects: serde_json::Value::Null,
            },
            Level::Error,
        ),
        (
            Failure::Unknown {
                reason: "unknown".into(),
                known_effects: serde_json::Value::Null,
            },
            Level::Error,
        ),
    ] {
        let e = trace.event(
            scope.clone(),
            Some("context".into()),
            Some("operation".into()),
            vec![],
            Fact::Completed {
                run_id: "run".into(),
                outcome: Outcome::Error { failure },
                origin: Origin::Runtime,
            },
        );
        assert_eq!(e.level, level);
        assert_eq!(e.granularity, Granularity::Operation);
        assert_eq!(e.version, VERSION);
        assert!(e.observed_at_unix_ms > 0);
    }
    assert!(trace.journal().records().unwrap().is_empty());
}

#[test]
fn cancelling_unpolled_future_never_submits_append() {
    let root = tempfile::tempdir().unwrap();
    for journal in [
        Journal::memory(),
        Journal::open(&root.path().join("trace")).unwrap(),
    ] {
        let e = event(&journal);
        let waiting = Box::pin(journal.append_async(e.clone()));
        drop(waiting);
        assert!(journal.records().unwrap().is_empty());
        assert_eq!(journal.append(e).unwrap().position.offset, 0);
    }
}

#[test]
fn durable_open_permissions_symlink_rejection_and_partial_parent_errors() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("new/trace");
    let journal = Journal::open(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(journal);
    let original = std::fs::read(&path).unwrap();
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(Journal::open(&link).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(Journal::open(&path.join("child")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn journal_rejects_fifos_without_blocking_or_changing_permissions() {
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    const CHILD: &str = "PVISOR_TEST_FIFO_OPERATION";
    if let Ok(operation) = std::env::var(CHILD) {
        let path = Path::new("fifo");
        let before = std::fs::metadata(path).unwrap().permissions().mode();
        match operation.as_str() {
            "read" => assert!(Journal::read(path).is_err()),
            "validate" => assert!(Journal::validate(path).is_err()),
            "open" => assert!(Journal::open(path).is_err()),
            _ => panic!("unknown operation"),
        }
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode(),
            before
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fifo");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is NUL-terminated and remains alive for the syscall.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o640) }, 0);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    for operation in ["read", "validate", "open"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "journal_rejects_fifos_without_blocking_or_changing_permissions",
                "--nocapture",
            ])
            .env(CHILD, operation)
            .current_dir(root.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            !timed_out && output.status.success(),
            "{operation}: timed_out={timed_out}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn atomic_write_accepts_a_bare_relative_filename() {
    // Isolate cwd in a subprocess so parallel tests never observe a cwd change.
    const CHILD: &str = "PVISOR_TEST_RELATIVE_ATOMIC_WRITE";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "atomic_write_accepts_a_bare_relative_filename",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let path = Path::new("record");
    Persistence::atomic_write(path, b"old", 0o600).unwrap();
    let mut phases = Vec::new();
    Persistence::atomic_write_observed(path, b"new", 0o640, |phase, _, ok| {
        phases.push((phase, ok));
    })
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"new");
    assert_eq!(phases.last(), Some(&("directory_sync", true)));
    assert!(phases.iter().all(|(_, ok)| *ok));
    assert_eq!(std::fs::read_dir(".").unwrap().count(), 1);
}

#[test]
fn atomic_write_handles_long_valid_destination_names() {
    let root = tempfile::tempdir().unwrap();
    for name in ["x".repeat(240), "é".repeat(120)] {
        let path = root.path().join(name);
        // Establish that the filesystem accepts this destination basename.
        std::fs::write(&path, b"old").unwrap();
        let mut phases = Vec::new();
        Persistence::atomic_write_observed(&path, b"new", 0o600, |phase, _, ok| {
            phases.push((phase, ok));
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(phases.last(), Some(&("directory_sync", true)));
        assert!(phases.iter().all(|(_, ok)| *ok));
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn durable_helpers_observe_after_cleanup_and_preserve_destinations_on_failure() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let leaf = root.path().join("a/b");
    Persistence::create_dir_all_durable(&leaf).unwrap();
    Persistence::create_dir_all_durable(&leaf).unwrap();
    Persistence::sync_directory(&leaf).unwrap();
    assert!(Persistence::sync_directory(&root.path().join("missing")).is_err());
    let path = leaf.join("record");
    Persistence::atomic_write(&path, b"old", 0o640).unwrap();
    let mut phases = vec![];
    Persistence::atomic_write_observed(&path, b"new", 0o600, |phase, duration, ok| {
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(
            std::fs::read_dir(&leaf).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        phases.push((phase, duration, ok));
    })
    .unwrap();
    assert_eq!(
        phases.iter().map(|p| (p.0, p.2)).collect::<Vec<_>>(),
        [
            ("directory_prepare", true),
            ("directory_prepare_sync", true),
            ("file_write", true),
            ("file_sync", true),
            ("rename", true),
            ("directory_sync", true)
        ]
    );
    assert_eq!(phases[1].1, std::time::Duration::ZERO);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let directory = leaf.join("destination");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("keep"), b"keep").unwrap();
    phases.clear();
    assert!(
        Persistence::atomic_write_observed(&directory, b"bad", 0o600, |phase, duration, ok| {
            assert!(
                std::fs::read_dir(&leaf).unwrap().all(|e| !e
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
            );
            phases.push((phase, duration, ok));
        })
        .is_err()
    );
    assert_eq!(phases.last().map(|p| (p.0, p.2)), Some(("rename", false)));
    assert_eq!(std::fs::read(directory.join("keep")).unwrap(), b"keep");
    assert!(Persistence::create_dir_all_durable(&path.join("child")).is_err());
    phases.clear();
    assert!(
        Persistence::atomic_write_observed(
            &path.join("child"),
            b"bad",
            0o600,
            |phase, duration, ok| phases.push((phase, duration, ok))
        )
        .is_err()
    );
    assert_eq!(
        phases.iter().map(|p| (p.0, p.2)).collect::<Vec<_>>(),
        [
            ("directory_prepare", false),
            ("directory_prepare_sync", false)
        ]
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

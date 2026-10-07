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

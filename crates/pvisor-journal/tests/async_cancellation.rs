//! A queued accepted append retains storage ownership after waiter cancellation.
use pvisor_core::event::{Durability, Fact};
use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};
use std::{
    future::Future,
    sync::mpsc,
    task::{Context, Poll, Waker},
    time::Duration,
};

#[test]
fn cancelled_queued_append_retains_writer_until_commit_and_retries_once() {
    for polls in [0, 1, 2] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let journal = Journal::open(&path).unwrap();
        let trace = Trace::with_id(journal.clone(), "queued-cancellation", "contract");
        let event = trace.event(
            vec!["run".into()],
            None,
            None,
            vec![],
            Fact::Observation {
                domain: "test".into(),
                name: "accepted-append".into(),
                version: 1,
                payload: serde_json::Value::Null,
            },
        );
        drop(trace);
        let mut live = journal.subscribe();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = runtime.spawn_blocking(move || {
            ready_tx.send(()).unwrap();
            // Also bound teardown if an earlier assertion fails.
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        {
            let _entered = runtime.enter();
            let mut waiting = Box::pin(journal.append_async(event.clone()));
            for _ in 0..polls {
                assert!(matches!(
                    waiting
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
            }
            drop(waiting);
        }
        drop(journal);

        if polls == 0 {
            assert!(Journal::read(&path).unwrap().is_empty());
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Closed)
            ));
        } else {
            // Only the accepted queued task can retain this writer and sender.
            let error = Journal::read(&path).unwrap_err();
            assert!(format!("{error:#}").contains("writer"), "{error:#}");
            assert!(matches!(
                live.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        }
        release_tx.send(()).unwrap();
        runtime.block_on(async {
            blocker.await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                if polls > 0 {
                    assert_eq!(live.recv().await.unwrap(), event);
                }
                assert!(matches!(
                    live.recv().await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed)
                ));
            })
            .await
            .unwrap();
        });
        drop(runtime);
        Journal::validate(&path).unwrap();
        let records = Journal::read(&path).unwrap();
        assert_eq!(records.len(), usize::from(polls > 0));
        if polls > 0 {
            assert_eq!(records[0].event, event);
        }
        let reopened = Journal::open(&path).unwrap();
        let mut after = reopened.subscribe();
        let receipt = reopened.append(event.clone()).unwrap();
        assert_eq!(receipt.position.offset, 0);
        assert_eq!(receipt.durability, Durability::LocalSync);
        if polls == 0 {
            assert_eq!(after.try_recv().unwrap(), event);
        } else {
            assert_eq!(receipt.position, records[0].position);
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(reopened.append(event).unwrap(), receipt);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(matches!(
            after.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(reopened.records().unwrap().len(), 1);
    }
}

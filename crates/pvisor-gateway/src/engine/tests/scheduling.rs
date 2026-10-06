//! Conventional scheduling regressions. Gates observe committed facts and
//! block only the observer thread, never a Tokio worker.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;

use super::fixtures::{RecordingSink, test_context, test_engine};
use super::support::*;
use crate::engine::{CancelEvent, CaptureEngine, StoryContext, load_story_snapshots};
use crate::record::CaptureRecord;
use crate::session::index::SessionIndexStore;
use crate::sink::CaptureEventObserver;

const WAIT: Duration = Duration::from_secs(10);

async fn assert_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

fn request(text: &str) -> Event {
    Event::Request(RequestEvent {
        path: "/v1/chat/completions".into(),
        method: "POST".into(),
        url: None,
        body_bytes: text.len(),
        user_content: Some(text.into()),
        body_json: None,
        semantic: None,
        model_rewritten: false,
        headers: vec![],
    })
}

fn complete(text: &str) -> Event {
    Event::ResponseComplete(CompleteEvent {
        status: 200,
        resp_bytes: Bytes::from_static(b"{}"),
        streaming: false,
        stream_metrics: None,
        assistant_content: Some(text.into()),
        semantic: None,
        headers: vec![],
    })
}

fn facts(storage: &std::path::Path) -> Vec<pvisor_core::event::Record> {
    pvisor_journal::Journal::read(&storage.join(".capture/events.trace.jsonl")).unwrap()
}

struct GatedSink {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    released: Mutex<bool>,
    wake: Condvar,
    journal: Option<pvisor_journal::Journal>,
    observed: Mutex<Vec<pvisor_core::event::Event>>,
    observed_ready: tokio::sync::Notify,
}

impl GatedSink {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }

    async fn wait_observed(&self, count: usize) {
        loop {
            let ready = self.observed_ready.notified();
            if self.observed.lock().unwrap().len() >= count {
                return;
            }
            ready.await;
        }
    }
}

impl CaptureEventObserver for GatedSink {
    fn journal(&self) -> Option<pvisor_journal::Journal> {
        self.journal.clone()
    }

    fn observe(&self, event: &pvisor_core::event::Event) -> anyhow::Result<()> {
        if CaptureRecord::from_event(event, 0)?.call_id.as_deref() == Some("hold") {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
        self.observed.lock().unwrap().push(event.clone());
        self.observed_ready.notify_one();
        Ok(())
    }
}

// Unblock spawn_blocking even if a test assertion unwinds.
struct ReleaseOnDrop(Arc<GatedSink>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn gated_engine(storage: &std::path::Path) -> (CaptureEngine, ReleaseOnDrop) {
    gated_engine_with_journal(storage, None).await
}

async fn gated_engine_with_journal(
    storage: &std::path::Path,
    journal: Option<pvisor_journal::Journal>,
) -> (CaptureEngine, ReleaseOnDrop) {
    let (entered, ready) = oneshot::channel();
    let sink = Arc::new(GatedSink {
        entered: Mutex::new(Some(entered)),
        released: Mutex::new(false),
        wake: Condvar::new(),
        journal,
        observed: Mutex::new(Vec::new()),
        observed_ready: tokio::sync::Notify::new(),
    });
    let release = ReleaseOnDrop(sink.clone());
    let index = SessionIndexStore::open(storage).unwrap().clone_handle();
    let engine = CaptureEngine::new(sink, index, Arc::new(storage.to_path_buf()))
        .await
        .unwrap();
    let mut ctx = test_context();
    ctx.call.call_id = "hold".into();
    engine.spawn_apply(ctx, request("gate"));
    tokio::time::timeout(WAIT, ready).await.unwrap().unwrap();
    (engine, release)
}

#[tokio::test]
async fn direct_apply_and_snapshot_follow_async_preparation_and_commit() {
    let dir = tempfile::tempdir().unwrap();
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), dir.path()).await;
    let ctx = test_context();
    engine.spawn_apply(ctx.clone(), request("first"));
    engine.apply(&ctx, complete("second")).await.unwrap();
    let snapshot = engine.story_snapshot(&ctx.story).await.unwrap();
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.turns[0].user.as_ref().unwrap().text, "first");
    assert_eq!(snapshot.turns[0].assistant.as_ref().unwrap().text, "second");
    let records = sink.drain();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind, "llm.request");
    assert_eq!(records[1].kind, "llm.response");
    engine.shutdown().await.unwrap();
    let records = facts(dir.path());
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[1].event.caused_by,
        vec![records[0].event.id.clone()]
    );
}

#[tokio::test]
async fn cancellation_of_admitted_apply_does_not_cancel_owned_work() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, release) = gated_engine(dir.path()).await;
    let ctx = test_context();
    let mut applying = Box::pin(engine.apply(&ctx, request("survives cancellation")));
    // One poll admits the job behind the gate and waits for its reply.
    assert_pending(applying.as_mut()).await;
    drop(applying);
    let mut cancelled = Box::pin(engine.apply(
        &ctx,
        Event::Cancelled(CancelEvent {
            reason: Some("client disconnected".into()),
            status: 200,
            bytes_received: 17,
            streaming: true,
        }),
    ));
    assert_pending(cancelled.as_mut()).await;
    drop(cancelled);
    release.0.release();
    tokio::time::timeout(WAIT, engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let records = facts(dir.path());
    assert_eq!(records.len(), 3);
    assert_eq!(records[1].event.name(), "llm.request");
    assert_eq!(records[2].event.name(), "llm.call.cancelled");
    assert_eq!(
        records[2].event.caused_by,
        vec![records[1].event.id.clone()]
    );
    let cancelled = CaptureRecord::from_event(&records[2].event, 0).unwrap();
    assert_eq!(cancelled.payload["reason"], "client disconnected");
    assert_eq!(cancelled.payload["bytes_received"], 17);
}

#[tokio::test]
async fn cancelled_flush_does_not_cancel_accepted_work_or_poison_later_barriers() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, release) = gated_engine(dir.path()).await;
    let ctx = test_context();
    engine.spawn_apply(ctx.clone(), request("before cancelled barrier"));
    let mut flushing = Box::pin(engine.flush());
    assert_pending(flushing.as_mut()).await;
    drop(flushing);
    engine.spawn_apply(ctx, complete("after cancelled barrier"));
    release.0.release();
    tokio::time::timeout(WAIT, engine.flush())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let records = facts(dir.path());
    assert_eq!(records.len(), 3);
    assert_eq!(records[1].event.name(), "llm.request");
    assert_eq!(records[2].event.name(), "llm.response");
    assert_eq!(
        records[2].event.caused_by,
        vec![records[1].event.id.clone()]
    );
}

#[tokio::test]
async fn dropping_last_runtime_without_shutdown_still_commits_accepted_tail() {
    let dir = tempfile::tempdir().unwrap();
    let journal =
        pvisor_journal::Journal::open(&dir.path().join(".capture/events.trace.jsonl")).unwrap();
    let (engine, release) = gated_engine_with_journal(dir.path(), Some(journal.clone())).await;
    let ctx = test_context();
    engine.spawn_apply(ctx.clone(), request("owned tail"));
    engine.spawn_apply(ctx, complete("owned completion"));
    // No runtime clone or flush future keeps scheduling ownership alive here.
    drop(engine);
    release.0.release();
    tokio::time::timeout(WAIT, release.0.wait_observed(3))
        .await
        .unwrap();
    let records = journal.records().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1].event.name(), "llm.request");
    assert_eq!(records[2].event.name(), "llm.response");
    assert_eq!(
        records[2].event.caused_by,
        vec![records[1].event.id.clone()]
    );
    let tail = CaptureRecord::from_event(&records[2].event, 0).unwrap();
    assert_eq!(tail.payload["assistant_content"], "owned completion");
}

#[tokio::test]
async fn blocked_story_does_not_block_another_story_or_flush_early() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, release) = gated_engine(dir.path()).await;
    let mut other = test_context();
    let mut route = other.route().clone();
    route.storage_session_id = "other".into();
    other.story = StoryContext::from_route(route, "other-agent");
    tokio::time::timeout(WAIT, engine.apply(&other, request("independent")))
        .await
        .unwrap()
        .unwrap();
    let mut flushing = Box::pin(engine.flush());
    assert_pending(flushing.as_mut()).await;
    release.0.release();
    tokio::time::timeout(WAIT, flushing).await.unwrap().unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(facts(dir.path()).len(), 2);
}

#[tokio::test]
async fn one_bounded_fifo_reports_overload_and_shutdown_drains_accepted_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, release) = gated_engine(dir.path()).await;
    // The owner is still awaiting the first observer. All 256 waiting slots
    // belong to this single FIFO; there is no second mailbox to hide overload.
    for i in 0..256 {
        let mut ctx = test_context();
        ctx.call.call_id = format!("accepted-{i}");
        engine.spawn_apply(ctx, request("accepted"));
    }
    let mut rejected = test_context();
    rejected.call.call_id = "rejected".into();
    engine.spawn_apply(rejected, request("overloaded"));
    let mut flushing = Box::pin(engine.flush());
    assert_pending(flushing.as_mut()).await;
    release.0.release();
    let error = tokio::time::timeout(WAIT, flushing)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("rejected job"));
    let retained_clone = engine.clone();
    let error = tokio::time::timeout(WAIT, engine.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("rejected job"));
    // Keep the writer alive: shutdown must drain diagnostics independently of Drop.
    assert_eq!(
        crate::dead_letter::read_dead_letter_entries(dir.path())
            .unwrap()
            .len(),
        1
    );
    drop(retained_clone);
    let records = facts(dir.path());
    assert_eq!(records.len(), 257);
    for (i, record) in records.iter().skip(1).enumerate() {
        assert_eq!(
            CaptureRecord::from_event(&record.event, 0).unwrap().call_id,
            Some(format!("accepted-{i}"))
        );
    }
    let snapshots = load_story_snapshots(dir.path()).unwrap();
    assert_eq!(snapshots["sess-1"].turns.len(), 257);
    let dead_letters = crate::dead_letter::read_dead_letter_entries(dir.path()).unwrap();
    assert_eq!(dead_letters.len(), 1);
    assert_eq!(dead_letters[0].context.call.call_id, "rejected");
}

#[tokio::test]
async fn rejected_journal_write_is_not_an_apply_or_flush_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(RecordingSink::new(), dir.path()).await;
    let ctx = test_context();
    let oversized = "x".repeat(pvisor_core::event::MAX_EVENT_BYTES);
    assert!(engine.apply(&ctx, request(&oversized)).await.is_err());
    assert!(
        engine
            .flush()
            .await
            .unwrap_err()
            .to_string()
            .contains("uncommitted events")
    );
    assert!(engine.shutdown().await.is_err());
    assert!(
        facts(dir.path()).is_empty(),
        "a rejected write cannot become a committed fact"
    );
    let entries = crate::dead_letter::read_dead_letter_entries(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].prepared_record_json.is_some());
}

#[tokio::test]
async fn failed_preparation_is_reported_without_abandoning_another_story() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(RecordingSink::new(), dir.path()).await;
    let mut invalid = test_context();
    invalid.request_headers = vec![("invalid\nheader".into(), "value".into())];
    engine.spawn_apply(invalid, request("rejected during prepare"));
    let mut other = test_context();
    let mut route = other.route().clone();
    route.storage_session_id = "other".into();
    other.story = StoryContext::from_route(route, "other-agent");
    engine.spawn_apply(other, request("must still drain"));
    let error = tokio::time::timeout(WAIT, engine.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted events"));
    let records = facts(dir.path());
    assert_eq!(records.len(), 1);
    assert_eq!(
        CaptureRecord::from_event(&records[0].event, 0)
            .unwrap()
            .payload["user_content"],
        "must still drain"
    );
    assert_eq!(
        load_story_snapshots(dir.path()).unwrap()["other"]
            .turns
            .len(),
        1
    );
    assert_eq!(
        crate::dead_letter::read_dead_letter_entries(dir.path())
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn shutdown_cancellation_still_joins_and_saves_final_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, release) = gated_engine(dir.path()).await;
    engine.spawn_apply(test_context(), request("tail"));
    let clone = engine.clone();
    let mut stopping = Box::pin(engine.shutdown());
    assert_pending(stopping.as_mut()).await;
    drop(stopping);
    // Polling the first future launches an owned shutdown task. Serialize a
    // second shutdown behind it to observe cleanup after cancellation.
    release.0.release();
    tokio::time::timeout(WAIT, clone.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(facts(dir.path()).len(), 2);
    assert_eq!(
        load_story_snapshots(dir.path()).unwrap()["sess-1"]
            .turns
            .len(),
        2
    );
}

#[tokio::test]
async fn shutdown_commits_cross_story_backfill_before_child_and_snapshots_main() {
    backfill_before_child(false).await;
}

#[tokio::test]
async fn same_owner_backfill_does_not_ask_its_own_fifo() {
    backfill_before_child(true).await;
}

async fn backfill_before_child(shared_story: bool) {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(RecordingSink::new(), dir.path()).await;
    let main = test_context();
    engine.apply(&main, request("delegate")).await.unwrap();
    engine.apply(&main, complete("```tool:Agent\n{\"description\":\"Review capture\",\"prompt\":\"Review docs/src/design/capture.md\",\"subagent_type\":\"general-purpose\"}\n```"))
        .await.unwrap();
    let mut child = test_context();
    child.call.call_id = "child-call".into();
    let mut route = child.route().clone();
    route.storage_session_id = if shared_story {
        "sess-1"
    } else {
        "agent-deadbeef"
    }
    .into();
    route.subagent_id = Some("deadbeef".into());
    child.story = StoryContext::from_route(route, "child-agent");
    let mut event = match request("Review docs/src/design/capture.md") {
        Event::Request(event) => event,
        _ => unreachable!(),
    };
    event.body_json = Some(
        serde_json::json!({"messages":[{"role":"user","content":"Review docs/src/design/capture.md"}]}),
    );
    engine.spawn_apply(child.clone(), Event::Request(event));
    tokio::time::timeout(WAIT, engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let records = facts(dir.path());
    assert_eq!(records.len(), 4);
    let backfill = CaptureRecord::from_event(&records[2].event, 0).unwrap();
    assert_eq!(backfill.kind, "llm.spawn_link");
    assert_eq!(
        backfill.call_id.as_deref(),
        Some(main.call.call_id.as_str())
    );
    assert_eq!(
        backfill.payload["spawn_links"][0]["subagent_id"],
        "deadbeef"
    );
    assert_eq!(
        crate::record::capture_observation(&records[2].event)
            .unwrap()
            .story
            .story_id,
        main.story.story_id
    );
    assert_eq!(
        crate::record::capture_observation(&records[3].event)
            .unwrap()
            .story
            .story_id,
        child.story.story_id
    );
    assert_eq!(
        records[2].event.caused_by,
        vec![records[0].event.id.clone()]
    );
    let snapshots = load_story_snapshots(dir.path()).unwrap();
    assert!(snapshots.contains_key("sess-1"));
    assert!(snapshots.contains_key(&child.route().storage_session_id));
}

#[tokio::test]
async fn failed_cross_story_backfill_retains_prepared_main_scope_and_drains_child() {
    failed_backfill_retains_prepared(false).await;
}

#[tokio::test]
async fn failed_inline_backfill_retains_prepared_main_scope_and_drains_child() {
    failed_backfill_retains_prepared(true).await;
}

async fn failed_backfill_retains_prepared(shared_story: bool) {
    let dir = tempfile::tempdir().unwrap();
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), dir.path()).await;
    let mut main = test_context();
    // Normal facts carry this id once and fit the real size limit. A backfill
    // repeats it in call_id, parent_call_id and payload, so only the link fails.
    main.call.call_id = "p".repeat(pvisor_core::event::MAX_EVENT_BYTES / 2);
    engine.apply(&main, request("delegate")).await.unwrap();
    engine.apply(&main, complete("```tool:Agent\n{\"description\":\"Review capture\",\"prompt\":\"Review docs/src/design/capture.md\",\"subagent_type\":\"general-purpose\"}\n```"))
        .await.unwrap();
    let mut child = test_context();
    child.call.call_id = "child-call".into();
    let mut route = child.route().clone();
    route.storage_session_id = if shared_story {
        "sess-1"
    } else {
        "agent-deadbeef"
    }
    .into();
    route.subagent_id = Some("deadbeef".into());
    child.story = StoryContext::from_route(route, "child-agent");
    let mut child_request = match request("Review docs/src/design/capture.md") {
        Event::Request(event) => event,
        _ => unreachable!(),
    };
    child_request.body_json = Some(
        serde_json::json!({"messages":[{"role":"user","content":"Review docs/src/design/capture.md"}]}),
    );
    let error = tokio::time::timeout(
        WAIT,
        engine.apply(&child, Event::Request(child_request.clone())),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("capture backfill has no receipt")
    );
    // Matching consumed the link: re-enriching the same child does not recreate
    // it. Its accepted source record and subsequent tail must still commit.
    engine
        .apply(&child, Event::Request(child_request))
        .await
        .unwrap();
    engine.spawn_apply(
        child.clone(),
        Event::Cancelled(CancelEvent {
            reason: Some("accepted child tail".into()),
            status: 200,
            bytes_received: 17,
            streaming: true,
        }),
    );
    assert!(
        tokio::time::timeout(WAIT, engine.flush())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        tokio::time::timeout(WAIT, engine.shutdown())
            .await
            .unwrap()
            .is_err()
    );
    let records = facts(dir.path());
    assert_eq!(records.len(), 5);
    assert!(
        records
            .iter()
            .all(|record| record.event.name() != "llm.spawn_link")
    );
    assert_eq!(records[2].event.name(), "llm.request");
    assert_eq!(records[3].event.name(), "llm.request");
    assert_eq!(records[4].event.name(), "llm.call.cancelled");
    assert_eq!(
        records[4].event.caused_by,
        vec![records[3].event.id.clone()]
    );
    assert_eq!(
        sink.drain().len(),
        5,
        "failed prepared link must not reach the observer"
    );
    let entries = crate::dead_letter::read_dead_letter_entries(dir.path()).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "retain the missing link, not the committed child"
    );
    assert_eq!(entries[0].context.route, *child.route());
    let target = entries[0].prepared_story.as_ref().unwrap();
    assert_eq!(target.story_id, main.story.story_id);
    assert_eq!(target.route, *main.route());
    assert_eq!(
        target.route.subagent_id, None,
        "inline retries still require main scope"
    );
    let retained: CaptureRecord =
        serde_json::from_str(entries[0].prepared_record_json.as_ref().unwrap()).unwrap();
    assert_eq!(retained.kind, "llm.spawn_link");
    assert_eq!(
        retained.call_id.as_deref(),
        Some(main.call.call_id.as_str())
    );
    assert_eq!(
        retained.parent_call_id.as_deref(),
        Some(main.call.call_id.as_str())
    );
    assert_eq!(
        retained.payload["spawn_links"][0]["subagent_id"],
        "deadbeef"
    );
    assert!(retained.event_id.is_some());
    assert!(retained.timestamp.is_some());
    assert!(entries[0].error.contains("event exceeds size limit"));
    let snapshots = load_story_snapshots(dir.path()).unwrap();
    assert!(snapshots.contains_key(&child.route().storage_session_id));
}

#[tokio::test]
async fn prepared_dead_letter_replay_keeps_identity_scope_and_does_not_reprepare_source() {
    use crate::engine::wire::{LocalStoryCommand, StoryScope};

    let dir = tempfile::tempdir().unwrap();
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), dir.path()).await;
    let main = test_context();
    engine.apply(&main, request("main request")).await.unwrap();
    let mut child = test_context();
    child.call.call_id = "uncommitted-source".into();
    let mut route = child.route().clone();
    route.storage_session_id = "agent-child".into();
    route.subagent_id = Some("child".into());
    child.story = StoryContext::from_route(route, "child-agent");
    let record =
        crate::subagent_link::spawn_link_backfill_record(&main.call.call_id, &[], &child.call);
    let command = LocalStoryCommand::persist_record(
        StoryScope {
            context: main.story.clone(),
        },
        record,
    );
    let LocalStoryCommand::PersistRecord { record, .. } = command else {
        unreachable!()
    };
    crate::dead_letter::append_prepared_dead_letter(
        dir.path(),
        &child,
        &request("do not replay this request"),
        "synthetic missing receipt",
        &main.story,
        &record,
    )
    .unwrap();
    for _ in 0..2 {
        let replay = crate::dead_letter::replay_dead_letter(dir.path(), &engine)
            .await
            .unwrap();
        assert_eq!(replay.attempted, 1);
        assert_eq!(replay.succeeded, 1);
        assert_eq!(replay.failed, 0);
    }
    engine.shutdown().await.unwrap();
    let records = facts(dir.path());
    assert_eq!(
        records.len(),
        2,
        "retry must not duplicate the link or replay the child request"
    );
    assert_eq!(records[1].event.name(), "llm.spawn_link");
    assert_eq!(Some(records[1].event.id.clone()), record.event_id);
    assert_eq!(
        Some(records[1].event.observed_at_unix_ms),
        record.observed_at_unix_ms
    );
    assert_eq!(
        crate::record::capture_observation(&records[1].event)
            .unwrap()
            .story,
        main.story
    );
    assert_eq!(
        records[1].event.caused_by,
        vec![records[0].event.id.clone()]
    );
    assert_eq!(
        sink.drain().len(),
        2,
        "retry receipt must not re-notify the observer"
    );
}

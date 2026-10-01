use std::sync::Arc;

use crate::dead_letter::read_trajectory_dead_letter_entries;
use crate::engine::{CaptureEngine, Event, RequestEvent};
use crate::session::index::SessionIndexStore;
use crate::sink::CaptureEventObserver;

use super::fixtures::test_context;

struct FailingSink;

impl CaptureEventObserver for FailingSink {
    fn observe(&self, _event: &pvisor_core::event::Event) -> anyhow::Result<()> {
        anyhow::bail!("observer unavailable")
    }
}

#[tokio::test]
async fn session_sink_failure_writes_dead_letter_with_record() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(dir.path().to_path_buf());
    let sink = Arc::new(FailingSink);
    let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
    let engine = CaptureEngine::new(sink, index, storage.clone(), false)
        .await
        .unwrap();
    let ctx = test_context();
    let event = Event::Request(RequestEvent {
        path: "/v1/chat/completions".into(),
        method: "POST".into(),
        url: None,
        body_bytes: 10,
        user_content: Some("hi".into()),
        body_json: None,
        semantic: None,
        model_rewritten: false,
        headers: vec![],
    });
    engine.apply(&ctx, event).await.unwrap();
    engine.flush().await.unwrap();
    let entries = read_trajectory_dead_letter_entries(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].error.contains("observer unavailable"));
    assert_eq!(entries[0].records[0].name(), "llm.request");
    let story = engine.story_snapshot(&ctx.story).await.unwrap();
    assert!(
        !story.turns.is_empty(),
        "failed observer must not undo a committed fact"
    );
}

#[tokio::test]
async fn observer_failure_does_not_erase_committed_facts() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(dir.path().to_path_buf());
    let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
    let engine = CaptureEngine::new(Arc::new(FailingSink), index, storage, false)
        .await
        .unwrap();
    engine.spawn_apply(
        test_context(),
        Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: None,
            body_bytes: 10,
            user_content: Some("retry me".into()),
            body_json: None,
            semantic: None,
            model_rewritten: false,
            headers: vec![],
        }),
    );
    engine.shutdown().await.unwrap();
    let records =
        pvisor_journal::Journal::read(&dir.path().join(".capture/events.trace.jsonl")).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.name(), "llm.request");
}

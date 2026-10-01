use super::fixtures::*;
use super::support::*;
use crate::engine::CaptureEngine;

#[tokio::test]
async fn committed_facts_rebuild_story_without_rewriting_or_notifying() {
    let dir = tempfile::tempdir().unwrap();
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    engine
        .apply(
            &test_context(),
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
                body_bytes: 5,
                user_content: Some("survives crash".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
            }),
        )
        .await
        .unwrap();
    engine.shutdown().await.unwrap();
    let path = dir.path().join(".capture/events.trace.jsonl");
    let before = persisting_journal::Journal::read(&path).unwrap();
    let sink = RecordingSink::new();
    let engine = test_engine(sink.clone(), dir.path(), false).await;
    let story = engine.story_snapshot(&test_context().story).await.unwrap();
    assert_eq!(story.turns.len(), 1);
    assert!(
        sink.drain().is_empty(),
        "recovery must not notify a second time"
    );
    engine.shutdown().await.unwrap();
    assert_eq!(before, persisting_journal::Journal::read(&path).unwrap());
}

#[tokio::test]
async fn requests_and_responses_share_causal_identity_after_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(RecordingSink::new(), dir.path(), false).await;
    engine
        .apply(
            &test_context(),
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
                body_bytes: 5,
                user_content: Some("hello".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
            }),
        )
        .await
        .unwrap();
    engine.shutdown().await.unwrap();
    let engine = test_engine(RecordingSink::new(), dir.path(), false).await;
    engine
        .apply(
            &test_context(),
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: bytes::Bytes::from_static(b"{}"),
                streaming: false,
                stream_metrics: None,
                assistant_content: Some("reply".into()),
                semantic: None,
                headers: vec![],
            }),
        )
        .await
        .unwrap();
    engine.shutdown().await.unwrap();
    let records =
        persisting_journal::Journal::read(&dir.path().join(".capture/events.trace.jsonl")).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[1].event.caused_by,
        vec![records[0].event.id.clone()]
    );
    let index = crate::session::index::SessionIndexStore::load(dir.path()).unwrap();
    assert_eq!(index.sessions[0].request_count, 1);
}

#[tokio::test]
async fn historical_wal_is_never_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".capture")).unwrap();
    let path = dir.path().join(".capture/events.wal.jsonl");
    std::fs::write(&path, "pending historical data\n").unwrap();
    let index = crate::session::index::SessionIndexStore::open(dir.path())
        .unwrap()
        .clone_handle();
    let result = CaptureEngine::new(
        RecordingSink::new(),
        index,
        std::sync::Arc::new(dir.path().to_path_buf()),
        false,
    )
    .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("previous version")
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "pending historical data\n"
    );
}

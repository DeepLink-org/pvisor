//! Per-story I/O actor — one mailbox per `story_id`, owns turn state and [`CaptureEventSink`] writes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pulsing_actor::prelude::*;

use super::super::story::{StoryId, TurnMachine};
use super::super::wire::{CaptureAck, StoryCommand, StoryReply};
use crate::sink::CaptureEventSink;

/// Injected sink for each story actor instance.
#[derive(Clone)]
pub(crate) struct StoryActorDeps {
    pub sink: Arc<dyn CaptureEventSink>,
    /// Retained so callers can keep passing the historical flag. Markdown
    /// projection no longer consumes it.
    #[allow(dead_code)]
    pub stream_markdown: bool,
    #[allow(dead_code)]
    pub storage: Arc<PathBuf>,
}

impl StoryActorDeps {
    pub fn new(
        sink: Arc<dyn CaptureEventSink>,
        storage: Arc<PathBuf>,
        stream_markdown: bool,
    ) -> Self {
        Self {
            sink,
            stream_markdown,
            storage,
        }
    }
}

/// Per-story actor — serializes I/O and maintains [`TurnMachine`] for the narrative index.
pub(crate) struct StoryActor {
    story_id: StoryId,
    deps: StoryActorDeps,
    turns: TurnMachine,
    storage_session_id: Option<String>,
}

impl StoryActor {
    pub fn new(story_id: StoryId, deps: StoryActorDeps) -> Self {
        let turns = TurnMachine::new(story_id.clone());
        Self {
            story_id,
            deps,
            turns,
            storage_session_id: None,
        }
    }

    fn sync_scope(&mut self, scope: &super::super::wire::StoryScope) {
        self.storage_session_id = Some(scope.route().storage_session_id.clone());
        self.turns
            .set_story_meta(scope.agent_id(), scope.context.run_id.clone());
    }

    async fn handle(&mut self, cmd: StoryCommand) -> Result<StoryReply> {
        match cmd {
            StoryCommand::Flush => {
                return Ok(StoryReply::Ack(CaptureAck::ok()));
            }
            StoryCommand::LocalSnapshot => {
                let storage_session_id = self
                    .storage_session_id
                    .clone()
                    .unwrap_or_else(|| self.story_id.as_str().to_string());
                return Ok(StoryReply::LocalSnapshot {
                    storage_session_id,
                    story: self.turns.snapshot(),
                });
            }
            StoryCommand::Snapshot { scope } => {
                self.sync_scope(&scope);
                return Ok(StoryReply::Snapshot {
                    story: self.turns.snapshot(),
                });
            }
            _ => {}
        }
        let scope = cmd.scope().clone();
        self.sync_scope(&scope);
        match cmd {
            StoryCommand::PersistRecord { record_bytes, .. } => {
                let mut rec: crate::record::EventRecord = serde_json::from_slice(&record_bytes)?;
                let mut next_turns = self.turns.clone();
                next_turns.observe_record(&mut rec);
                let sink = Arc::clone(&self.deps.sink);
                let route = scope.route().clone();
                let agent_id = scope.agent_id().to_string();
                tokio::task::spawn_blocking(move || {
                    sink.append(&route, &agent_id, &mut rec)
                        .context("capture append")?;
                    Ok::<_, anyhow::Error>(())
                })
                .await
                .context("join capture append")??;
                self.turns = next_turns;
            }
            StoryCommand::UpsertDraft { .. } => {}
            StoryCommand::Flush | StoryCommand::Snapshot { .. } | StoryCommand::LocalSnapshot => {
                unreachable!()
            }
        }
        Ok(StoryReply::Ack(CaptureAck::ok()))
    }
}

#[async_trait]
impl Actor for StoryActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("story_id".into(), self.story_id.as_str().to_string()),
            ("turns".into(), self.turns.turns().len().to_string()),
        ])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: StoryCommand = msg.unpack()?;
        let reply = match self.handle(cmd).await {
            Ok(r) => r,
            Err(e) => StoryReply::Ack(CaptureAck::err(format!("{e:#}"))),
        };
        Message::pack(&reply)
    }
}

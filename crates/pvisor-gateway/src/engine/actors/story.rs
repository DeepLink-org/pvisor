//! Per-story I/O actor — one mailbox per `story_id`, owns turn state and [`CaptureEventObserver`] writes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pulsing_actor::prelude::*;

use super::super::story::{StoryId, TurnMachine};
use super::super::wire::{CaptureAck, StoryCommand, StoryReply};
use crate::sink::CaptureEventObserver;

/// Injected sink for each story actor instance.
#[derive(Clone)]
pub(crate) struct StoryActorDeps {
    pub sink: Arc<dyn CaptureEventObserver>,
    pub storage: Arc<PathBuf>,
    pub journal: pvisor_journal::Journal,
    pub index: crate::session::index::SessionIndexHandle,
    requests: Arc<std::sync::Mutex<HashMap<(String, String), String>>>,
}

impl StoryActorDeps {
    pub fn new(
        sink: Arc<dyn CaptureEventObserver>,
        storage: Arc<PathBuf>,
        journal: pvisor_journal::Journal,
        index: crate::session::index::SessionIndexHandle,
    ) -> Self {
        Self {
            sink,
            storage,
            journal,
            index,
            requests: Default::default(),
        }
    }
}

/// Per-story actor — serializes I/O and maintains [`TurnMachine`] for the narrative index.
pub(crate) struct StoryActor {
    story_id: StoryId,
    deps: StoryActorDeps,
    turns: TurnMachine,
    storage_session_id: Option<String>,
    seen: HashSet<String>,
}

impl StoryActor {
    pub fn new(story_id: StoryId, deps: StoryActorDeps) -> Self {
        let turns = TurnMachine::new(story_id.clone());
        Self {
            story_id,
            deps,
            turns,
            storage_session_id: None,
            seen: HashSet::new(),
        }
    }

    fn sync_scope(&mut self, scope: &super::super::wire::StoryScope) {
        self.storage_session_id = Some(scope.route().storage_session_id.clone());
        self.turns
            .set_story_meta(scope.agent_id(), scope.context.run_id.clone());
    }

    fn remember_request(
        &self,
        rec: &crate::record::CaptureRecord,
        story: &crate::engine::StoryContext,
    ) {
        if rec.kind == "llm.request"
            && let (Some(call), Some(id)) = (&rec.call_id, &rec.event_id)
        {
            let root = story
                .run_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or(story.story_id.as_str());
            self.deps
                .requests
                .lock()
                .unwrap()
                .insert((root.to_string(), call.clone()), id.clone());
        }
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
            StoryCommand::Restore { record_bytes, .. } => {
                let mut rec: crate::record::CaptureRecord = serde_json::from_slice(&record_bytes)?;
                self.remember_request(&rec, &scope.context);
                if self.seen.insert(
                    rec.event_id
                        .clone()
                        .context("restored record missing identity")?,
                ) {
                    self.turns.observe_record(&mut rec);
                }
            }
            StoryCommand::PersistRecord { record_bytes, .. } => {
                let rec: crate::record::CaptureRecord = serde_json::from_slice(&record_bytes)?;
                let mut event = rec.clone().into_event(scope.context.clone())?;
                let root = event.trace_id.clone();
                let dependency = if rec.kind == "llm.request" {
                    rec.parent_call_id.as_ref()
                } else {
                    rec.call_id.as_ref()
                };
                if let Some(call) = dependency
                    && let Some(cause) = self
                        .deps
                        .requests
                        .lock()
                        .unwrap()
                        .get(&(root, call.clone()))
                {
                    event.caused_by.push(cause.clone());
                }
                let receipt = self
                    .deps
                    .journal
                    .append_async(event.clone())
                    .await
                    .context("capture commit")?;
                if !self.seen.insert(event.id.clone()) {
                    return Ok(StoryReply::Ack(CaptureAck::ok()));
                }
                let mut committed =
                    crate::record::CaptureRecord::from_event(&event, receipt.position.offset)?;
                self.remember_request(&committed, &scope.context);
                self.turns.observe_record(&mut committed);
                self.deps.index.observe_event(&event)?;
                let observer = Arc::clone(&self.deps.sink);
                let storage = self.deps.storage.clone();
                let story = scope.context.clone();
                let diagnostic = event.clone();
                let observed = tokio::task::spawn_blocking(move || {
                    if let Err(error) = observer.observe(&event) {
                        tracing::warn!("post-commit capture observer failed: {error:#}");
                        crate::dead_letter::append_trajectory_dead_letter(
                            storage.as_path(),
                            &story.agent_id,
                            &story.route.session_id,
                            story.route.root_session.as_deref(),
                            &[diagnostic],
                            &error.to_string(),
                        )?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
                if let Err(error) = observed
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result)
                {
                    tracing::warn!("post-commit observer diagnostics failed: {error:#}");
                }
            }
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

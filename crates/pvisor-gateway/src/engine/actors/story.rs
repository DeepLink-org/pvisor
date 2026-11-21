//! Story state and I/O, owned by the per-story scheduler.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pvisor_journal::api::{Journal, JournalStore};

use super::super::story::{StoryId, TurnMachine};
use super::super::wire::{LocalStoryCommand, StoryReply};
use crate::sink::CaptureEventObserver;

/// Dependencies shared by story scheduling owners.
#[derive(Clone)]
pub(crate) struct StoryActorDeps {
    pub sink: Arc<dyn CaptureEventObserver>,
    pub storage: Arc<PathBuf>,
    pub journal: Journal,
    pub index: crate::session::index::SessionIndexHandle,
    requests: Arc<std::sync::Mutex<HashMap<(String, String), String>>>,
}

impl StoryActorDeps {
    pub fn new(
        sink: Arc<dyn CaptureEventObserver>,
        storage: Arc<PathBuf>,
        journal: Journal,
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

/// Story state and I/O maintained by its single scheduling owner.
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
    ) -> Result<()> {
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
                .map_err(|_| anyhow::anyhow!("capture request registry mutex poisoned"))?
                .insert((root.to_string(), call.clone()), id.clone());
        }
        Ok(())
    }

    pub(crate) async fn handle(&mut self, cmd: LocalStoryCommand) -> Result<StoryReply> {
        match cmd {
            LocalStoryCommand::Flush => {
                return Ok(StoryReply::Ack);
            }
            LocalStoryCommand::LocalSnapshot => {
                let storage_session_id = self
                    .storage_session_id
                    .clone()
                    .unwrap_or_else(|| self.story_id.as_str().to_string());
                return Ok(StoryReply::LocalSnapshot {
                    storage_session_id,
                    story: self.turns.snapshot(),
                });
            }
            LocalStoryCommand::Snapshot { scope } => {
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
            LocalStoryCommand::Restore {
                record: mut rec, ..
            } => {
                self.remember_request(&rec, &scope.context)?;
                if self.seen.insert(
                    rec.event_id
                        .clone()
                        .context("restored record missing identity")?,
                ) {
                    self.turns.observe_record(&mut rec);
                }
            }
            LocalStoryCommand::PersistRecord { record: rec, .. } => {
                let mut event = rec.clone().into_event(scope.context.clone())?;
                let root = event.trace_id.clone();
                let dependency = if rec.kind == "llm.request" {
                    rec.parent_call_id.as_ref()
                } else {
                    rec.call_id.as_ref()
                };
                let cause = {
                    let requests =
                        self.deps.requests.lock().map_err(|_| {
                            anyhow::anyhow!("capture request registry mutex poisoned")
                        })?;
                    dependency.and_then(|call| requests.get(&(root, call.clone())).cloned())
                };
                if let Some(cause) = cause {
                    event.caused_by.push(cause);
                }
                let receipt = self
                    .deps
                    .journal
                    .append_async(event.clone())
                    .await
                    .context("capture commit")?;
                if !self.seen.insert(event.id.clone()) {
                    return Ok(StoryReply::Ack);
                }
                let mut committed =
                    crate::record::CaptureRecord::from_event(&event, receipt.position.offset)?;
                self.remember_request(&committed, &scope.context)?;
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
            LocalStoryCommand::Flush
            | LocalStoryCommand::Snapshot { .. }
            | LocalStoryCommand::LocalSnapshot => {
                unreachable!()
            }
        }
        Ok(StoryReply::Ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::root_request(None)]
    #[case::child_request(Some("parent"))]
    #[tokio::test]
    async fn poisoned_request_registry_rejects_before_commit(#[case] parent: Option<&str>) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("events.trace.jsonl")).unwrap();
        let index = crate::session::index::SessionIndexStore::open(dir.path()).unwrap();
        let deps = StoryActorDeps::new(
            Arc::new(crate::sink::NoopCaptureObserver::new()),
            Arc::new(dir.path().to_path_buf()),
            journal.clone(),
            index.clone_handle(),
        );
        assert!(
            std::panic::catch_unwind(|| {
                let _guard = deps.requests.lock().unwrap();
                panic!("injected request registry failure");
            })
            .is_err()
        );
        let context = crate::engine::StoryContext::from_route(
            crate::session::storage::CaptureRoute {
                root_session: Some("run".into()),
                session_id: "s".into(),
                storage_session_id: "run".into(),
                subagent_id: None,
            },
            "a",
        );
        let mut actor = StoryActor::new(context.story_id.clone(), deps);
        let record = serde_json::from_value(serde_json::json!({
            "kind": "llm.request", "event_id": "fixed", "payload": {},
            "call_id": "c", "parent_call_id": parent
        }))
        .unwrap();
        let command = LocalStoryCommand::persist_record(
            super::super::super::wire::StoryScope { context },
            record,
        );
        for _ in 0..2 {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                actor.handle(command.clone()),
            )
            .await
            .unwrap();
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("request registry mutex poisoned")
            );
        }
        assert!(journal.records().unwrap().is_empty());
        assert!(actor.seen.is_empty());
    }
}

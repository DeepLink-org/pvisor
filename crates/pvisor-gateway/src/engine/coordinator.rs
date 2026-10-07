//! Capture coordinator: one bounded scheduling owner per story.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::actors::{RunActor, StoryActor, StoryActorDeps};
use super::apply_queue::ApplyDispatcher;
use super::egress::persist_story_snapshots;
use super::prepare::CapturePreparer;
use super::story::{Story, StoryContext};
use super::wire::{LocalStoryCommand, StoryReply, StoryScope, run_main_route};
use super::{CallContext, Event};
use crate::dead_letter;
use crate::session::index::SessionIndexHandle;
use crate::sink::CaptureEventObserver;
use crate::subagent_link::spawn_link_backfill_record;
use pvisor_journal::api::{Journal, JournalStore};

pub(crate) struct CaptureRuntimeInner {
    preparer: CapturePreparer,
    // Enrichment is synchronous. Never hold this lock over a Journal wait or
    // a cross-story dispatch; the registry cannot become a scheduling cycle.
    run: Mutex<RunActor>,
    shutdown: tokio::sync::Mutex<()>,
    pub(crate) story_deps: StoryActorDeps,
}

/// Each story owner performs preparation, Journal commit and projection I/O in
/// FIFO order. Direct apply, async capture and read/barrier commands share it.
#[derive(Clone)]
pub struct CaptureRuntime {
    inner: Arc<CaptureRuntimeInner>,
    apply_dispatcher: ApplyDispatcher,
}

impl CaptureRuntime {
    pub async fn new(
        sink: Arc<dyn CaptureEventObserver>,
        index: SessionIndexHandle,
        storage: Arc<PathBuf>,
    ) -> Result<Self> {
        let journal = match sink.journal() {
            Some(journal) => journal,
            None => Journal::open(&storage.join(".capture").join("events.trace.jsonl"))?,
        };
        let inner = Arc::new(CaptureRuntimeInner {
            preparer: CapturePreparer {
                storage: Arc::clone(&storage),
            },
            run: Mutex::new(RunActor::new()),
            shutdown: tokio::sync::Mutex::new(()),
            story_deps: StoryActorDeps::new(sink, storage, journal, index),
        });
        let apply_dispatcher = ApplyDispatcher::new(Arc::clone(&inner));
        let runtime = Self {
            inner,
            apply_dispatcher,
        };
        runtime.rebuild_projections().await?;
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn dispatcher_for_test(&self) -> &ApplyDispatcher {
        &self.apply_dispatcher
    }

    /// Replay immutable committed facts; never re-run HTTP or prepare commands.
    async fn rebuild_projections(&self) -> Result<()> {
        let journal = self.inner.story_deps.journal.clone();
        let records = tokio::task::spawn_blocking(move || journal.records()).await??;
        self.inner.story_deps.index.rebuild_from_events(&records)?;
        for record in records {
            if !crate::record::is_capture_event(&record.event) {
                continue;
            }
            let payload = crate::record::capture_observation(&record.event)?;
            let scope = StoryScope {
                context: payload.story,
            };
            let rec =
                crate::record::CaptureRecord::from_event(&record.event, record.position.offset)?;
            let story_id = scope.context.story_id.as_str().to_string();
            story_reply_ack(
                self.apply_dispatcher
                    .command(&story_id, LocalStoryCommand::Restore { scope, record: rec })
                    .await?,
            )?;
        }
        Ok(())
    }

    /// Non-blocking, bounded admission. Queue acceptance is not durable
    /// acceptance: only Journal receipts prove persistence. Rejection records
    /// a capture gap, with best-effort bounded dead-letter reporting.
    pub fn spawn_apply(&self, ctx: impl Into<Arc<CallContext>>, event: Event) {
        self.apply_dispatcher.enqueue(ctx.into(), event);
    }

    /// Backpressured admission to the same FIFO as `spawn_apply`. Once admitted,
    /// dropping this future does not cancel the owned capture job.
    pub async fn apply(&self, ctx: &CallContext, event: Event) -> Result<()> {
        self.apply_dispatcher
            .apply(Arc::new(ctx.clone()), event)
            .await
    }

    /// Retry retained prepared input through the same bounded owner without
    /// re-running enrichment. Stamping preserves the original retry identity.
    pub(crate) async fn apply_prepared_record(
        &self,
        story: StoryContext,
        record: crate::record::CaptureRecord,
    ) -> Result<()> {
        let story_id = story.story_id.as_str().to_string();
        let command = LocalStoryCommand::persist_record(StoryScope { context: story }, record);
        story_reply_ack(self.apply_dispatcher.command(&story_id, command).await?)
    }

    /// Drain accepted capture work and rejected-event diagnostics queued before
    /// the writer marker, even with other runtime clones alive. Diagnostic
    /// completion is not fsync durability and excludes rejected/late diagnostics.
    pub async fn shutdown(self) -> Result<()> {
        // Once polled, shutdown owns its cleanup even if the caller cancels the
        // wait. Dropping a JoinHandle detaches rather than aborts this drain.
        tokio::spawn(async move {
            let _shutdown = self.inner.shutdown.lock().await;
            let (drained, snapshots) = self.apply_dispatcher.shutdown().await;
            // A capture gap must not abandon another story's accepted tail or
            // prevent final projection/index persistence.
            let persisted =
                persist_story_snapshots(self.inner.story_deps.storage.as_path(), &snapshots);
            let indexed = self.inner.story_deps.index.flush_if_dirty();
            drained?;
            persisted?;
            indexed?;
            Ok(())
        })
        .await?
    }

    /// Barrier behind accepted jobs, including the backfills they await. This
    /// is not a fence against producers that continue admitting new work.
    /// Rejected-event diagnostics use a separate writer; shutdown, not this
    /// capture barrier, explicitly drains that writer.
    pub async fn flush(&self) -> Result<()> {
        let _shutdown = self.inner.shutdown.lock().await;
        self.apply_dispatcher.flush().await?;
        self.inner.story_deps.index.flush_if_dirty()?;
        Ok(())
    }

    pub async fn story_snapshot(&self, context: &StoryContext) -> Result<Story> {
        let reply = self
            .apply_dispatcher
            .command(
                context.story_id.as_str(),
                LocalStoryCommand::Snapshot {
                    scope: StoryScope {
                        context: context.clone(),
                    },
                },
            )
            .await?;
        match reply {
            StoryReply::Snapshot { story } | StoryReply::LocalSnapshot { story, .. } => Ok(story),
            StoryReply::Ack => Err(anyhow::anyhow!("unexpected ack for snapshot")),
        }
    }
}

impl CaptureRuntimeInner {
    pub(crate) async fn apply_to_story(
        &self,
        dispatcher: &ApplyDispatcher,
        story: &mut StoryActor,
        ctx: &CallContext,
        event: Event,
    ) -> Result<()> {
        let mut prepared = match self.preparer.prepare(&self.run, ctx, event.clone()).await {
            Ok(p) => p,
            Err(e) => {
                record_dead_letter(self.story_deps.storage.as_path(), ctx, &event, &e, None);
                return Err(e);
            }
        };

        let mut backfill_error = None;
        if !prepared.backfills.is_empty() {
            let main = run_main_route(&self.run, prepared.ctx.route());
            let scope = StoryScope {
                context: StoryContext::from_route(main, ctx.agent_id()),
            };
            for bf in &prepared.backfills {
                let mut rec = spawn_link_backfill_record(&bf.parent_call_id, &bf.links, &ctx.call);
                crate::sink::retain_capture_content(&mut rec.payload, ctx.level);
                let cmd = LocalStoryCommand::persist_record(scope.clone(), rec);
                // Backfills flow only from subagents to the main story, never
                // the reverse. Same-owner work must not ask its own bounded FIFO.
                let reply = if &scope.context.story_id == ctx.story_id() {
                    story.handle(cmd.clone()).await
                } else {
                    dispatcher
                        .command_internal(scope.context.story_id.as_str(), cmd.clone())
                        .await
                };
                // A missing receipt is a capture gap, not merely a diagnostic.
                if let Err(error) = reply.and_then(story_reply_ack) {
                    dispatcher.record_failure(&error);
                    if let LocalStoryCommand::PersistRecord { scope, record } = &cmd
                        && let Err(dl) = dead_letter::append_prepared_dead_letter(
                            self.story_deps.storage.as_path(),
                            ctx,
                            &event,
                            &format!("{error:#}"),
                            &scope.context,
                            record,
                        )
                    {
                        // Diagnostic failure must not abandon the child's accepted job.
                        tracing::error!("prepared backfill dead letter write failed: {dl:#}");
                    }
                    tracing::warn!("spawn link backfill failed: {error:#}");
                    backfill_error.get_or_insert(error);
                }
            }
        }

        if let Some(cmd) = prepared.take_story_command() {
            let result = story.handle(cmd.clone()).await.and_then(story_reply_ack);
            if let Err(e) = result {
                // Serialize only on the diagnostic boundary, not for dispatch.
                let record_json = match cmd {
                    LocalStoryCommand::PersistRecord { record, .. } => {
                        serde_json::to_string(&record).ok()
                    }
                    _ => None,
                };
                record_dead_letter(
                    self.story_deps.storage.as_path(),
                    ctx,
                    &event,
                    &e,
                    record_json,
                );
                return Err(e);
            }
        }
        match backfill_error {
            Some(error) => Err(error.context("capture backfill has no receipt")),
            None => Ok(()),
        }
    }
}

pub type CaptureEngine = CaptureRuntime;

pub(crate) fn story_reply_ack(reply: StoryReply) -> Result<()> {
    match reply {
        StoryReply::Ack => Ok(()),
        StoryReply::Snapshot { .. } | StoryReply::LocalSnapshot { .. } => {
            Err(anyhow::anyhow!("unexpected snapshot reply"))
        }
    }
}

fn record_dead_letter(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &anyhow::Error,
    prepared_record_json: Option<String>,
) {
    if let Err(dl) = dead_letter::append_dead_letter(
        storage,
        ctx,
        event,
        &format!("{error:#}"),
        prepared_record_json,
    ) {
        tracing::error!("dead letter write failed: {dl:#}");
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    use crate::engine::story::{StoryId, TurnMachine};

    #[test]
    fn typed_ack_requires_an_ack_reply() {
        assert!(story_reply_ack(StoryReply::Ack).is_ok());
        let story = TurnMachine::new(StoryId::new("s")).snapshot();
        for reply in [
            StoryReply::Snapshot {
                story: story.clone(),
            },
            StoryReply::LocalSnapshot {
                storage_session_id: "s".into(),
                story,
            },
        ] {
            let error = story_reply_ack(reply).unwrap_err();
            assert_eq!(error.to_string(), "unexpected snapshot reply");
        }
    }
}

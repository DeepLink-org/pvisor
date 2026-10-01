use anyhow::Result;
use async_trait::async_trait;
use pvisor_core::event::{Event, Fact, Receipt};
use pvisor_core::{AttemptId, RunId};
use pvisor_journal::{AppendError, Journal, Trace};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAppendErrorKind {
    Rejected,
    Unknown,
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn append(&self, event: &Event) -> Result<Receipt>;
    fn journal(&self) -> Option<Journal> {
        None
    }
    fn classify_append_error(&self, error: &anyhow::Error) -> EventAppendErrorKind {
        match error.downcast_ref::<AppendError>() {
            Some(AppendError::Rejected(_)) => EventAppendErrorKind::Rejected,
            _ => EventAppendErrorKind::Unknown,
        }
    }
}

#[async_trait]
impl EventSink for Journal {
    fn journal(&self) -> Option<Journal> {
        Some(self.clone())
    }
    async fn append(&self, event: &Event) -> Result<Receipt> {
        Ok(self.append_async(event.clone()).await?)
    }
}

/// Recording disabled: events still pass validation and receive volatile receipts.
#[derive(Default)]
pub struct NoopEventSink {
    journal: Journal,
}
#[async_trait]
impl EventSink for NoopEventSink {
    fn journal(&self) -> Option<Journal> {
        Some(self.journal.clone())
    }
    async fn append(&self, event: &Event) -> Result<Receipt> {
        Ok(self.journal.append_async(event.clone()).await?)
    }
}

#[derive(Default)]
pub struct MemoryEventSink {
    journal: Journal,
}
impl MemoryEventSink {
    pub fn events(&self) -> Vec<Event> {
        self.journal
            .records()
            .expect("memory journal")
            .into_iter()
            .map(|r| r.event)
            .collect()
    }
}
#[async_trait]
impl EventSink for MemoryEventSink {
    fn journal(&self) -> Option<Journal> {
        Some(self.journal.clone())
    }
    async fn append(&self, event: &Event) -> Result<Receipt> {
        Ok(self.journal.append_async(event.clone()).await?)
    }
}

/// One ordered producer; positions and durability belong to the fact journal.
#[derive(Clone)]
pub struct RunEventPublisher {
    trace: Trace,
    scope: Vec<String>,
    context_id: String,
    operation_id: String,
    cause: Arc<Mutex<Option<String>>>,
    sink: Arc<dyn EventSink>,
    live: broadcast::Sender<Event>,
}
impl RunEventPublisher {
    pub(crate) fn new(
        run_id: RunId,
        attempt_id: AttemptId,
        producer: impl Into<String>,
        sink: Arc<dyn EventSink>,
        live: broadcast::Sender<Event>,
    ) -> Self {
        let mut trace = Trace::new(Journal::memory(), producer);
        trace.id = run_id.to_string();
        Self {
            trace,
            scope: vec![
                "run".into(),
                run_id.to_string(),
                "attempt".into(),
                attempt_id.to_string(),
            ],
            context_id: uuid::Uuid::new_v4().to_string(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            cause: Arc::new(Mutex::new(None)),
            sink,
            live,
        }
    }
    pub fn subscribe(&self) -> super::run::RunEventStream {
        let receiver = match self.sink.journal() {
            Some(journal) => journal.subscribe(),
            None => self.live.subscribe(),
        };
        super::run::RunEventStream {
            trace_id: self.trace.id.clone(),
            receiver,
        }
    }
    pub(crate) fn classify_append_error(&self, error: &anyhow::Error) -> EventAppendErrorKind {
        self.sink.classify_append_error(error)
    }
    pub async fn publish(
        &self,
        kind: impl Into<String>,
        source: impl Into<String>,
        payload: Value,
    ) -> Result<Event> {
        let mut cause = self.cause.lock().await;
        let event = self.trace.event(
            self.scope.clone(),
            None,
            None,
            cause.iter().cloned().collect(),
            Fact::Observation {
                domain: source.into(),
                name: kind.into(),
                version: 1,
                payload,
            },
        );
        let receipt = self.sink.append(&event).await?;
        anyhow::ensure!(
            receipt.event == event.id,
            "sink returned a receipt for a different event"
        );
        *cause = Some(event.id.clone());
        if self.sink.journal().is_none() {
            let _ = self.live.send(event.clone());
        }
        Ok(event)
    }
    pub async fn publish_fact(&self, data: Fact) -> Result<Event> {
        let mut cause = self.cause.lock().await;
        let operation = if matches!(data, Fact::Context { .. }) {
            None
        } else {
            Some(self.operation_id.clone())
        };
        let event = self.trace.event(
            self.scope.clone(),
            Some(self.context_id.clone()),
            operation,
            cause.iter().cloned().collect(),
            data,
        );
        let receipt = self.sink.append(&event).await?;
        anyhow::ensure!(
            receipt.event == event.id,
            "sink returned an unrelated receipt"
        );
        *cause = Some(event.id.clone());
        if self.sink.journal().is_none() {
            let _ = self.live.send(event.clone());
        }
        Ok(event)
    }
    pub async fn begin_execution(
        &self,
        requested: &pvisor_core::operation::Operation,
        operation: &pvisor_core::operation::Operation,
        backend: &str,
    ) -> Result<()> {
        let mut context = operation.context.clone();
        context.scope = self.scope.clone();
        self.publish_fact(Fact::Context {
            definition: context,
        })
        .await?;
        self.publish_fact(Fact::Requested {
            operation: requested.clone(),
        })
        .await?;
        let mut effective = operation.clone();
        effective.placements.clear();
        if requested != &effective {
            self.publish_fact(Fact::Rewritten {
                before: requested.clone(),
                after: Box::new(effective),
            })
            .await?;
        }
        self.publish_fact(Fact::Placed {
            operation: operation.clone(),
        })
        .await?;
        self.publish_fact(Fact::Dispatched {
            backend: backend.to_string(),
            run_id: operation.run_id.clone(),
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn run_subscription_includes_gateway_but_filters_other_runs_and_retries() {
        let journal = Journal::memory();
        let (live, _) = broadcast::channel(4);
        let publisher = RunEventPublisher::new(
            "run".into(),
            "attempt".into(),
            "runtime",
            Arc::new(journal.clone()),
            live,
        );
        let mut stream = publisher.subscribe();
        let mut trace = Trace::new(journal.clone(), "pvisor-gateway");
        let fact = Fact::Observation {
            domain: "llm".into(),
            name: "llm.request".into(),
            version: 1,
            payload: Value::Null,
        };
        trace.id = "other-run".into();
        journal
            .append(trace.event(vec!["capture".into()], None, None, vec![], fact.clone()))
            .unwrap();
        trace.id = "run".into();
        let event = trace.event(vec!["capture".into()], None, None, vec![], fact);
        journal.append(event.clone()).unwrap();
        journal.append(event.clone()).unwrap();
        assert_eq!(stream.recv().await.unwrap(), event);
        assert!(matches!(
            stream.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
}

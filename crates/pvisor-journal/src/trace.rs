use crate::api::{Journal, JournalStore, TraceProducer};
use anyhow::Result;
use pvisor_core::event::{Event, Fact, Granularity, Level, VERSION};

/// A producer for one trace. Contexts and operations have independent identities.
#[derive(Clone)]
pub struct Trace {
    journal: Journal,
    id: String,
    producer: String,
}

impl TraceProducer for Trace {
    fn new(journal: Journal, producer: impl Into<String>) -> Self {
        Self {
            journal,
            id: uuid::Uuid::new_v4().to_string(),
            producer: producer.into(),
        }
    }
    fn with_id(journal: Journal, id: impl Into<String>, producer: impl Into<String>) -> Self {
        Self {
            journal,
            id: id.into(),
            producer: producer.into(),
        }
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn producer(&self) -> &str {
        &self.producer
    }
    fn journal(&self) -> &Journal {
        &self.journal
    }
    fn event(
        &self,
        scope: Vec<String>,
        context: Option<String>,
        operation: Option<String>,
        caused_by: Vec<String>,
        data: Fact,
    ) -> Event {
        let granularity = match data {
            Fact::Context { .. } => Granularity::Detail,
            _ => Granularity::Operation,
        };
        let level = match &data {
            Fact::Completed {
                outcome: pvisor_core::operation::Outcome::Error { failure },
                ..
            } => match failure {
                pvisor_core::operation::Failure::Denied { .. }
                | pvisor_core::operation::Failure::Unsupported { .. } => Level::Warn,
                _ => Level::Error,
            },
            _ => Level::Info,
        };
        Event {
            version: VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            trace_id: self.id.clone(),
            producer: self.producer.clone(),
            observed_at_unix_ms: pvisor_core::unix_now_ms(),
            scope,
            context,
            operation,
            caused_by,
            level,
            granularity,
            data,
        }
    }
    async fn emit(&self, event: Event) -> Result<String> {
        let receipt = self.journal.append_async(event).await?;
        Ok(receipt.event)
    }
}

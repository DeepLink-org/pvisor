//! Events distinguish the immutable Run plan, execution dispatch and
//! observed results. Event identity, operation identity and position are separate.
use crate::operation::{Context, Operation, Outcome};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u16 = 5;
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Milestone,
    Operation,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: u16,
    pub id: String,
    pub trace_id: String,
    pub producer: String,
    pub observed_at_unix_ms: u64,
    pub scope: Vec<String>,
    pub context: Option<String>,
    pub operation: Option<String>,
    pub caused_by: Vec<String>,
    pub level: Level,
    pub granularity: Granularity,
    pub data: Fact,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fact {
    Context {
        definition: Context,
    },
    Requested {
        operation: Operation,
    },
    Rewritten {
        before: Operation,
        after: Operation,
    },
    Placed {
        operation: Operation,
    },
    Dispatched {
        backend: String,
        run_id: String,
    },
    Completed {
        run_id: String,
        outcome: Outcome,
        origin: Origin,
    },
    Observation {
        domain: String,
        name: String,
        version: u16,
        payload: Value,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Backend,
    Policy,
    Replay,
    Runtime,
}
impl Fact {
    pub fn domain(&self) -> &str {
        match self {
            Self::Observation { domain, .. } => domain,
            Self::Context { .. } => "execution",
            Self::Requested { .. }
            | Self::Rewritten { .. }
            | Self::Placed { .. }
            | Self::Dispatched { .. }
            | Self::Completed { .. } => "run",
        }
    }
}
impl Event {
    /// Domain observation name; execution phases use their fact name.
    pub fn name(&self) -> &str {
        match &self.data {
            Fact::Observation { name, .. } => name,
            Fact::Context { .. } => "context",
            Fact::Requested { .. } => "requested",
            Fact::Rewritten { .. } => "rewritten",
            Fact::Placed { .. } => "placed",
            Fact::Dispatched { .. } => "dispatched",
            Fact::Completed { .. } => "completed",
        }
    }
    pub fn observation_payload(&self) -> Option<&Value> {
        match &self.data {
            Fact::Observation { payload, .. } => Some(payload),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == VERSION, "unsupported trace event version");
        for id in [&self.id, &self.trace_id, &self.producer] {
            ensure!(
                !id.trim().is_empty() && id.len() <= 256,
                "invalid event identity"
            );
        }
        ensure!(
            !self.scope.is_empty()
                && self.scope.len() <= 16
                && self
                    .scope
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 256),
            "invalid event scope"
        );
        ensure!(self.caused_by.len() <= 64, "too many causal references");
        let mut seen = std::collections::BTreeSet::new();
        for id in &self.caused_by {
            ensure!(
                !id.is_empty() && id.len() <= 256 && id != &self.id && seen.insert(id),
                "invalid causal reference"
            );
        }
        for id in [&self.context, &self.operation].into_iter().flatten() {
            ensure!(
                !id.is_empty() && id.len() <= 256,
                "invalid execution reference"
            );
        }
        match &self.data {
            Fact::Context { definition } => {
                definition.validate()?;
                ensure!(
                    self.context.is_some()
                        && self.operation.is_none()
                        && self.scope == definition.scope,
                    "invalid context fact"
                );
            }
            Fact::Requested { operation } | Fact::Placed { operation } => operation.validate()?,
            Fact::Rewritten { before, after } => {
                before.validate()?;
                after.validate()?;
                ensure!(
                    before.run_id == after.run_id && before.kind.name() == after.kind.name(),
                    "rewrite changes operation identity or kind"
                );
                ensure!(
                    before.placements == after.placements,
                    "rewrite changes placement"
                );
            }
            Fact::Dispatched { backend, run_id } => {
                ensure!(!backend.trim().is_empty(), "invalid backend");
                ensure!(!run_id.is_empty(), "empty Run identity");
            }
            Fact::Completed {
                run_id, outcome, ..
            } => {
                ensure!(!run_id.is_empty(), "empty Run identity");
                outcome.validate()?;
            }
            Fact::Observation {
                domain,
                name,
                version,
                ..
            } => {
                crate::operation::symbol(domain)?;
                crate::operation::symbol(name)?;
                ensure!(*version > 0, "invalid observation version");
            }
        }
        if !matches!(self.data, Fact::Context { .. } | Fact::Observation { .. }) {
            ensure!(
                self.operation.is_some() && self.context.is_some(),
                "operation fact requires operation and context"
            );
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_EVENT_BYTES,
            "event exceeds size limit"
        );
        Ok(())
    }

    /// Human-readable projection. The structured event remains the stored record.
    pub fn to_text(&self) -> Result<String> {
        self.validate()?;
        let body = match &self.data {
            Fact::Context { definition } => format!(
                "context principal={} policy={} revision={}",
                serde_json::to_string(&definition.principal)?,
                serde_json::to_string(&definition.policy)?,
                definition.revision
            ),
            Fact::Requested { operation } => {
                format!("requested {}", serde_json::to_string(operation)?)
            }
            Fact::Rewritten { before, after } => format!(
                "rewritten {} => {}",
                serde_json::to_string(before)?,
                serde_json::to_string(after)?
            ),
            Fact::Placed { operation } => format!("placed {}", serde_json::to_string(operation)?),
            Fact::Dispatched { backend, run_id } => format!(
                "dispatched backend={} run={}",
                serde_json::to_string(backend)?,
                serde_json::to_string(run_id)?
            ),
            Fact::Completed {
                run_id,
                outcome,
                origin,
            } => format!(
                "completed run={} => {} origin={}",
                serde_json::to_string(run_id)?,
                serde_json::to_string(outcome)?,
                serde_json::to_string(origin)?
            ),
            Fact::Observation {
                domain,
                name,
                version,
                payload,
            } => format!(
                "{domain}.{name}@{version} {}",
                serde_json::to_string(payload)?
            ),
        };
        Ok(format!(
            "op={} {body}",
            serde_json::to_string(&self.operation)?
        ))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub journal: String,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    Volatile,
    LocalSync,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub event: String,
    pub position: Position,
    pub durability: Durability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub position: Position,
    pub event: Event,
}

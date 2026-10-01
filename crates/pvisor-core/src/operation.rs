//! Immutable Operation definition and its boundary observations.
//! The runtime executes RunSpec; this schema describes placement and evidence.
use crate::execution::{CapabilityDimension, EnforcementPlan};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};

pub const OPERATION_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Run {
        state: crate::execution::RunState,
        exit_code: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success { value: Value },
    Error { failure: Failure },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Failure {
    Failed {
        domain: String,
        code: String,
        effects: Json,
    },
    Denied {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
    Unknown {
        reason: String,
        known_effects: Json,
    },
}
impl Outcome {
    pub fn success(value: Value) -> Self {
        Self::Success { value }
    }
    pub fn validate(&self) -> Result<()> {
        if let Self::Success {
            value: Value::Run { state, .. },
        } = self
        {
            ensure!(state.is_terminal(), "run result requires a terminal state");
        }
        Ok(())
    }
}

/// Trusted execution metadata is outside the execution mechanism. A binding is
/// a description of a capability; the backend must check actual authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub backend: String,
    pub resource: String,
    pub generation: u64,
    pub contract: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub revision: u64,
    pub principal: String,
    pub scope: Vec<String>,
    pub policy: String,
    #[serde(deserialize_with = "unique_map")]
    pub bindings: BTreeMap<String, Binding>,
}
impl Context {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.principal.trim().is_empty() && !self.policy.trim().is_empty(),
            "context requires principal and policy"
        );
        ensure!(
            !self.scope.is_empty()
                && self.scope.len() <= 16
                && self
                    .scope
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 256),
            "invalid scope"
        );
        for (name, binding) in &self.bindings {
            ensure!(
                !name.is_empty()
                    && !binding.backend.is_empty()
                    && !binding.resource.is_empty()
                    && binding.contract == 1,
                "invalid resource binding"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "placement", rename_all = "snake_case", deny_unknown_fields)]
pub enum Placement {
    Vm { name: String },
    Overlay { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDecision {
    pub id: String,
    pub dimension: CapabilityDimension,
    pub target: String,
    /// The effective access decision, such as deny, ask, read, stage or write.
    pub action: String,
    pub enforcement_plan: EnforcementPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum OperationKind {
    #[serde(rename = "run.execute")]
    RunExecute {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
    },
}
impl OperationKind {
    pub fn name(&self) -> &'static str {
        "run.execute"
    }
    fn validate(&self) -> Result<()> {
        let Self::RunExecute { program, .. } = self;
        ensure!(!program.trim().is_empty(), "empty operation program");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub version: u16,
    pub context: Context,
    pub run_id: String,
    pub kind: OperationKind,
    /// Selected placements, ordered inner-to-outer; these do not execute or grant authority.
    pub placements: Vec<Placement>,
    pub rules: Vec<OperationDecision>,
}

impl Operation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == OPERATION_VERSION,
            "unsupported Operation version"
        );
        ensure!(!self.run_id.is_empty(), "empty Run identity");
        self.kind.validate()?;
        self.context.validate()?;
        ensure!(self.placements.len() <= 32, "too many placements");
        for placement in &self.placements {
            let (Placement::Vm { name } | Placement::Overlay { name }) = placement;
            ensure!(!name.trim().is_empty(), "empty placement name");
        }
        ensure!(self.rules.len() <= 1024, "too many Operation rules");
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            symbol(&rule.id)?;
            ensure!(ids.insert(&rule.id), "duplicate Operation rule {}", rule.id);
            ensure!(
                !rule.target.is_empty() && !rule.action.is_empty(),
                "empty Operation rule"
            );
        }
        Ok(())
    }
}

/// `None` means the boundary cannot observe this quantity. Zero means it can
/// observe it and saw no matching operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCounters {
    pub allowed: Option<u64>,
    pub succeeded: Option<u64>,
    pub denied: Option<u64>,
    pub failed: Option<u64>,
    pub effects: Option<u64>,
    /// Mutating operations that failed after execution began may have partial effects.
    pub uncertain_effects: Option<u64>,
    pub bytes_read: Option<u64>,
    pub bytes_written: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathOperationCounters {
    pub hits: u64,
    pub allowed: u64,
    pub succeeded: u64,
    pub denied: u64,
    pub failed: u64,
    pub effects: u64,
    pub uncertain_effects: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemObservation {
    /// Mount-relative paths; only operations that reached the FUSE boundary.
    pub paths: BTreeMap<String, BTreeMap<String, PathOperationCounters>>,
    /// Exact overlay access rules matched by observed operations.
    pub rules: BTreeMap<String, PathOperationCounters>,
    /// Operations omitted after the bounded path table filled.
    pub overflow_hits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationObservation {
    pub outcome: Outcome,
    pub rules: BTreeMap<String, RuleCounters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemObservation>,
}

impl OperationObservation {
    pub fn validate(&self, plan: &Operation) -> Result<()> {
        plan.validate()?;
        self.outcome.validate()?;
        let ids: BTreeSet<_> = plan.rules.iter().map(|rule| rule.id.as_str()).collect();
        ensure!(
            self.rules.keys().all(|id| ids.contains(id.as_str())),
            "observation references an unknown rule"
        );
        ensure!(
            self.rules.len() == ids.len(),
            "observation omits a Operation rule"
        );
        if let Some(filesystem) = &self.filesystem {
            ensure!(filesystem.paths.len() <= 8192, "too many observed paths");
            ensure!(
                filesystem.rules.keys().all(|id| ids.contains(id.as_str())),
                "filesystem observation references an unknown rule"
            );
            for counters in filesystem
                .paths
                .values()
                .flat_map(|operations| operations.values())
                .chain(filesystem.rules.values())
            {
                ensure!(
                    counters.allowed.saturating_add(counters.denied) == counters.hits,
                    "filesystem hit counts do not reconcile"
                );
                ensure!(
                    counters.succeeded.saturating_add(counters.failed) == counters.allowed,
                    "filesystem outcomes do not reconcile"
                );
                ensure!(
                    counters.effects <= counters.succeeded
                        && counters.uncertain_effects <= counters.failed,
                    "filesystem effect counts exceed outcomes"
                );
            }
        }
        Ok(())
    }
}
pub fn symbol(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "invalid symbol {name:?}"
    );
    Ok(())
}

fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

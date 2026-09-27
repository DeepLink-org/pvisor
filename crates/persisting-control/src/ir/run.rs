//! Resolved Run policy and observations carried by the same IR as operations.
//! A plan is immutable; observations never change the authority it declares.

use super::{Context, Expression, Operation, Outcome, Rule, symbol};
use crate::runtime::{CapabilityDimension, EnforcementEvidence};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const RUN_PLAN_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRule {
    pub id: String,
    pub dimension: CapabilityDimension,
    pub target: String,
    /// The effective access decision, such as deny, ask, read, stage or write.
    pub action: String,
    pub enforcement: EnforcementEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPlan {
    pub version: u16,
    pub context: Context,
    pub request: Expression,
    /// Ordered IR suffix rewrites used to derive the execution placement.
    pub rewrites: Vec<Rule>,
    pub expression: Expression,
    pub rules: Vec<PlanRule>,
}

impl RunPlan {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == RUN_PLAN_VERSION,
            "unsupported Run plan version"
        );
        self.context.validate()?;
        self.request.validate()?;
        self.expression.validate()?;
        ensure!(
            matches!(&self.expression.operation, Operation::Run { .. }),
            "Run plan requires run.execute IR"
        );
        let mut derived = self.request.clone();
        for rule in &self.rewrites {
            derived = rule.apply(&derived)?;
        }
        ensure!(
            derived == self.expression,
            "Run plan expression differs from its IR rewrites"
        );
        ensure!(self.rules.len() <= 1024, "too many Run plan rules");
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            symbol(&rule.id)?;
            ensure!(ids.insert(&rule.id), "duplicate Run plan rule {}", rule.id);
            ensure!(
                !rule.target.is_empty() && !rule.action.is_empty(),
                "empty Run plan rule"
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
pub struct RunObservation {
    pub outcome: Outcome,
    pub rules: BTreeMap<String, RuleCounters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemObservation>,
}

impl RunObservation {
    pub fn validate(&self, plan: &RunPlan) -> Result<()> {
        plan.validate()?;
        plan.expression.operation.check_outcome(&self.outcome)?;
        let ids: BTreeSet<_> = plan.rules.iter().map(|rule| rule.id.as_str()).collect();
        ensure!(
            self.rules.keys().all(|id| ids.contains(id.as_str())),
            "observation references an unknown rule"
        );
        ensure!(
            self.rules.len() == ids.len(),
            "observation omits a Run plan rule"
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

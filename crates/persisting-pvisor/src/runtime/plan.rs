//! Project the admitted RunSpec and runtime boundary into immutable Run IR.
//! Execution and policy enforcement remain in the Run runtime and its drivers.

use super::OverlayHint;
use persisting_control::{
    CapabilityDimension, CapabilityEnforcementPlan, ExecutorKind, ExecutorPlan, FilesystemAccess,
    NetworkCapability, RunResult, RunSpec, RunState,
    ir::run::{
        FilesystemObservation, PlanRule, RUN_PLAN_VERSION, RuleCounters, RunObservation, RunPlan,
    },
    ir::{
        Binding, Context, Expression, Failure, Layer, OpCode, Operation, Outcome, Pattern, Rewrite,
        Rule, Value,
    },
};
use std::collections::BTreeMap;

pub(crate) fn compile(
    spec: &RunSpec,
    executor: &ExecutorPlan,
    evidence: &CapabilityEnforcementPlan,
    overlay: &OverlayHint,
) -> anyhow::Result<RunPlan> {
    let request = Expression::new(Operation::Run {
        run_id: spec.run_id.as_str().to_owned(),
    });
    let mut expression = request.clone();
    let mut rewrites = Vec::new();
    if executor.kind == ExecutorKind::VirtualMachine {
        append_placement(
            &mut expression,
            &mut rewrites,
            "placement.vm",
            Layer::Vm {
                name: executor.name.clone(),
            },
        )?;
    }
    if overlay.stage_dir.is_some() {
        append_placement(
            &mut expression,
            &mut rewrites,
            "placement.overlay",
            Layer::Overlay {
                name: "pvisor-stage".into(),
            },
        )?;
    }
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "executor".into(),
        Binding {
            backend: executor.name.clone(),
            resource: spec.run_id.as_str().to_owned(),
            generation: spec.lease_epoch,
            contract: 1,
        },
    );
    let context = Context {
        revision: spec.lease_epoch,
        principal: spec.agent.name.clone(),
        scope: vec!["run".into(), spec.run_id.as_str().to_owned()],
        policy: format!("{:?}", spec.runtime.policy_mode).to_lowercase(),
        bindings,
    };
    let mut rules = Vec::new();
    let mut push = |id: String, dimension, target: String, action: &str| {
        rules.push(PlanRule {
            id,
            dimension,
            target,
            action: action.into(),
            enforcement_plan: evidence
                .dimensions
                .get(&dimension)
                .cloned()
                .unwrap_or_default(),
        });
    };
    push(
        "run.execute".into(),
        CapabilityDimension::Subprocess,
        spec.run_id.as_str().into(),
        "execute",
    );
    if let Some(stage) = &overlay.stage_dir {
        let target = overlay
            .merged_dir
            .as_ref()
            .or_else(|| overlay.lower_dirs.first());
        push(
            "fs.stage".into(),
            CapabilityDimension::FilesystemWrite,
            target.map_or_else(
                || stage.display().to_string(),
                |path| path.display().to_string(),
            ),
            "stage",
        );
    }
    for (index, grant) in spec.capabilities.filesystem.iter().enumerate() {
        let writable = grant.access == FilesystemAccess::ReadWrite;
        push(
            format!("fs.grant.{index}"),
            if writable {
                CapabilityDimension::FilesystemWrite
            } else {
                CapabilityDimension::FilesystemRead
            },
            grant.path.clone(),
            if writable { "write" } else { "read" },
        );
    }
    for (id, path, action) in spec.policies.filesystem(&overlay.access_policy).rules() {
        push(id, CapabilityDimension::FilesystemRead, path, action);
    }
    let network_action = match &spec.capabilities.network {
        NetworkCapability::Ambient => "ambient",
        NetworkCapability::Deny => "deny",
        NetworkCapability::AllowList { .. }
        | NetworkCapability::Policy { .. }
        | NetworkCapability::Scoped { .. } => "policy",
    };
    push(
        "net.aggregate".into(),
        CapabilityDimension::Network,
        "egress".into(),
        network_action,
    );
    match &spec.capabilities.network {
        NetworkCapability::AllowList {
            hosts,
            rules: network_rules,
        } => {
            for (index, host) in hosts.iter().enumerate() {
                push(
                    format!("net.allow.host.{index}"),
                    CapabilityDimension::Network,
                    host.clone(),
                    "allow",
                );
            }
            for (index, rule) in network_rules.iter().enumerate() {
                push(
                    format!("net.allow.rule.{index}"),
                    CapabilityDimension::Network,
                    serde_json::to_string(rule)?,
                    "allow",
                );
            }
        }
        NetworkCapability::Policy {
            allow,
            deny,
            limits,
            ..
        } => {
            for (index, rule) in allow.iter().enumerate() {
                push(
                    format!("net.allow.rule.{index}"),
                    CapabilityDimension::Network,
                    serde_json::to_string(rule)?,
                    "allow",
                );
            }
            for (index, rule) in deny.iter().enumerate() {
                push(
                    format!("net.deny.rule.{index}"),
                    CapabilityDimension::Network,
                    serde_json::to_string(rule)?,
                    "deny",
                );
            }
            for (index, limit) in limits.iter().enumerate() {
                push(
                    format!("net.limit.{index}"),
                    CapabilityDimension::Network,
                    serde_json::to_string(limit)?,
                    "limit",
                );
            }
        }
        NetworkCapability::Scoped { layers, fallback } => {
            push(
                "net.scopes".into(),
                CapabilityDimension::Network,
                serde_json::to_string(&spec.capabilities.network)?,
                "policy",
            );
            for (scope, layer) in layers {
                for (action, entries) in [("allow", &layer.allow), ("deny", &layer.deny)] {
                    for (index, rule) in entries.iter().enumerate() {
                        push(
                            format!("{scope:?}.net.{action}.rule.{index}").to_lowercase(),
                            CapabilityDimension::Network,
                            serde_json::to_string(rule)?,
                            action,
                        );
                    }
                }
            }
            push(
                "net.fallback".into(),
                CapabilityDimension::Network,
                serde_json::to_string(fallback)?,
                "policy",
            );
        }
        _ => {}
    }
    let persisting_control::RunInvocation::Process(process) = &spec.invocation;
    push(
        "env.projection".into(),
        CapabilityDimension::Secrets,
        "environment".into(),
        if process.inherit_env {
            "inherit"
        } else {
            "project"
        },
    );
    for (index, key) in process.env.keys().enumerate() {
        push(
            format!("env.key.{index}"),
            CapabilityDimension::Secrets,
            key.clone(),
            "project",
        );
    }
    let plan = RunPlan {
        version: RUN_PLAN_VERSION,
        context,
        request,
        rewrites,
        expression,
        rules,
    };
    plan.validate()?;
    Ok(plan)
}

fn append_placement(
    expression: &mut Expression,
    rewrites: &mut Vec<Rule>,
    id: &str,
    layer: Layer,
) -> anyhow::Result<()> {
    let rule = Rule {
        id: id.into(),
        version: 1,
        pattern: Pattern {
            operation: OpCode::Run,
            file: Some(expression.operation.file().into()),
            contexts: Some(expression.contexts.clone()),
        },
        rewrite: Rewrite::Append {
            contexts: vec![layer],
        },
    };
    *expression = rule.apply(expression)?;
    rewrites.push(rule);
    Ok(())
}

pub(crate) fn observe(
    plan: &RunPlan,
    result: &RunResult,
    network: Option<&persisting_overlaynet::InterceptionSnapshot>,
    filesystem: Option<&FilesystemObservation>,
) -> anyhow::Result<RunObservation> {
    let outcome = if result.state == RunState::Completed {
        Outcome::success(Value::Run {
            state: RunState::Completed,
            exit_code: result.exit_code,
        })
    } else {
        Outcome::Error {
            failure: Failure::Failed {
                domain: "run".into(),
                code: format!("{:?}", result.state).to_lowercase(),
                effects: serde_json::json!({"exit_code": result.exit_code, "failure": result.failure}),
            },
        }
    };
    let mut rules = plan
        .rules
        .iter()
        .map(|rule| (rule.id.clone(), RuleCounters::default()))
        .collect::<BTreeMap<_, _>>();
    rules.insert(
        "run.execute".into(),
        RuleCounters {
            allowed: Some(1),
            succeeded: Some(u64::from(result.state == RunState::Completed)),
            denied: Some(0),
            failed: Some(u64::from(result.state != RunState::Completed)),
            ..RuleCounters::default()
        },
    );
    if let Some(network) = network {
        rules.insert(
            "net.aggregate".into(),
            RuleCounters {
                allowed: Some(network.policy_allowed),
                succeeded: None,
                denied: Some(network.policy_denied),
                // Drivers may count one failure in both categories; use the larger
                // observed count rather than asserting a false sum.
                failed: Some(network.failures.max(network.tcp_connect_failures)),
                effects: None,
                uncertain_effects: None,
                bytes_read: Some(network.bytes_host_to_guest),
                bytes_written: Some(network.bytes_guest_to_host),
            },
        );
    }
    if let Some(filesystem) = filesystem {
        for rule in &plan.rules {
            if rule.id == "fs.stage"
                || rule.id.starts_with("fs.deny.")
                || rule.id.starts_with("fs.ask.")
                || rule.id.starts_with("fs.warn.")
            {
                let count = filesystem.rules.get(&rule.id).cloned().unwrap_or_default();
                rules.insert(
                    rule.id.clone(),
                    RuleCounters {
                        allowed: Some(count.allowed),
                        succeeded: Some(count.succeeded),
                        denied: Some(count.denied),
                        failed: Some(count.failed),
                        effects: Some(count.effects),
                        uncertain_effects: Some(count.uncertain_effects),
                        bytes_read: Some(count.bytes_read),
                        bytes_written: Some(count.bytes_written),
                    },
                );
            }
        }
    }
    let observation = RunObservation {
        outcome,
        rules,
        filesystem: filesystem.cloned(),
    };
    observation.validate(plan)?;
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PVisor;

    #[test]
    fn production_observation_rejects_invalid_filesystem_counts() {
        let spec = RunSpec::process("observation-check", "agent", "/bin/true");
        let plan = PVisor::new().resolve_run_plan(spec).unwrap();
        let result: RunResult = serde_json::from_value(serde_json::json!({
            "run_id": "observation-check", "attempt_id": "attempt",
            "state": "completed", "started_at_unix_ms": 0,
            "finished_at_unix_ms": 1, "exit_code": 0
        }))
        .unwrap();
        assert!(observe(&plan, &result, None, None).is_ok());
        let mut filesystem = FilesystemObservation::default();
        filesystem.paths.entry("file".into()).or_default().insert(
            "read".into(),
            persisting_control::ir::run::PathOperationCounters {
                hits: 1,
                ..Default::default()
            },
        );
        assert!(observe(&plan, &result, None, Some(&filesystem)).is_err());
    }
}

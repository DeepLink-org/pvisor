//! Request -> suffix rewrites -> backend/terminal handler -> observed result.
//! The original expression is never mutated or reconstructed from the final one.
use crate::trace::Trace;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use persisting_control::{
    ir::{Context, Expression, Failure, Layer, Outcome, Rule},
    trace::{Fact, Origin},
};

/// Implement the complete, ordered context chain. Reject unsupported combinations;
/// do not silently drop a vm/remote/overlay wrapper or flatten their order.
#[async_trait]
pub trait Backend: Send {
    fn name(&self) -> &str;
    fn authorize(&self, context: &Context, expression: &Expression) -> Result<(), Failure>;
    async fn execute(
        &mut self,
        context: &Context,
        expression: &Expression,
        execution: &ExecutionContext,
    ) -> Outcome;
}

pub struct ExecutionContext {
    pub trace: Trace,
    pub context_id: String,
    pub operation_id: String,
    pub dispatch_event: String,
}

/// Includes request-supplied contexts and pure mock/deny handlers. Syntax itself
/// grants no capability. Final backend authorization sees the entire derived chain.
pub trait Admission: Send + Sync {
    fn admit(&self, context: &Context, request: &Expression) -> Result<(), String>;
    fn rewrite(
        &self,
        context: &Context,
        rule: &Rule,
        before: &Expression,
        after: &Expression,
    ) -> Result<(), String>;
    fn deliver(
        &self,
        context: &Context,
        request: &Expression,
        outcome: &Outcome,
    ) -> Result<(), String>;
}
#[derive(Debug)]
pub struct Execution {
    pub operation_id: String,
    pub expression: Expression,
    pub outcome: Outcome,
    /// A known result survives a post-effect audit failure; callers must inspect gaps.
    pub audit_errors: Vec<String>,
}
pub struct Engine<B, A> {
    pub backend: B,
    pub admission: A,
    pub trace: Trace,
    pub passes: Vec<Vec<Rule>>,
}

impl<B: Backend, A: Admission> Engine<B, A> {
    pub async fn run(&mut self, context: &Context, request: &Expression) -> Result<Execution> {
        context.validate()?;
        request.validate()?;
        ensure!(
            self.passes.len() <= 32 && self.passes.iter().map(Vec::len).sum::<usize>() <= 1024,
            "rewrite configuration exceeds limits"
        );
        for rule in self.passes.iter().flatten() {
            rule.validate()?;
        }
        let context_id = uuid::Uuid::new_v4().to_string();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let context_event = self.trace.event(
            context.scope.clone(),
            Some(context_id.clone()),
            None,
            vec![],
            Fact::Context {
                definition: context.clone(),
            },
        );
        let mut cause = self.trace.emit(context_event).await?;
        let event = |data, causes| {
            self.trace.event(
                context.scope.clone(),
                Some(context_id.clone()),
                Some(operation_id.clone()),
                causes,
                data,
            )
        };
        cause = self
            .trace
            .emit(event(
                Fact::Requested {
                    request: request.clone(),
                },
                vec![cause],
            ))
            .await?;
        let mut expression = request.clone();
        let mut origin = Origin::Runtime;
        let mut outcome = self
            .admission
            .admit(context, request)
            .err()
            .map(Outcome::denied);
        if outcome.is_none() {
            for (pass, rules) in self.passes.iter().enumerate() {
                let Some(rule) = rules.iter().find(|rule| rule.pattern.matches(&expression)) else {
                    continue;
                };
                let after = match rule.apply(&expression) {
                    Ok(after) => after,
                    Err(error) => {
                        outcome = Some(Outcome::Error {
                            failure: Failure::Failed {
                                domain: "policy".into(),
                                code: "invalid_rewrite".into(),
                                effects: serde_json::json!({"kind":"none", "reason":error.to_string()}),
                            },
                        });
                        break;
                    }
                };
                if let Err(reason) = self.admission.rewrite(context, rule, &expression, &after) {
                    outcome = Some(Outcome::denied(reason));
                    break;
                }
                let fact = Fact::Rewritten {
                    rule: rule.clone(),
                    pass,
                    before: expression.clone(),
                    after: after.clone(),
                };
                cause = self.trace.emit(event(fact, vec![cause])).await?;
                expression = after;
            }
        }
        if outcome.is_none() {
            if let Some(value) = expression.contexts.last().and_then(Layer::terminal) {
                origin = Origin::Policy;
                outcome = Some(value);
            } else if let Err(failure) = self.backend.authorize(context, &expression) {
                outcome = Some(Outcome::Error { failure });
            } else {
                let fact = Fact::Dispatched {
                    backend: self.backend.name().into(),
                    expression: expression.clone(),
                };
                cause = self.trace.emit(event(fact, vec![cause])).await?;
                let execution = ExecutionContext {
                    trace: self.trace.clone(),
                    context_id: context_id.clone(),
                    operation_id: operation_id.clone(),
                    dispatch_event: cause.clone(),
                };
                origin = Origin::Backend;
                outcome = Some(self.backend.execute(context, &expression, &execution).await);
            }
        }
        let mut outcome = outcome.expect("every admitted request has a handler");
        if let Err(error) = expression
            .operation
            .check_outcome(&outcome)
            .and_then(|()| request.operation.check_outcome(&outcome))
        {
            outcome = Outcome::Error {
                failure: Failure::Unknown {
                    reason: error.to_string(),
                    known_effects: serde_json::json!({"reported_outcome":outcome}),
                },
            };
        }
        let mut audit_errors = Vec::new();
        if let Err(reason) = self.admission.deliver(context, request, &outcome) {
            let fact = Fact::Observation {
                domain: "execution".into(),
                name: "delivery_denied".into(),
                version: 1,
                payload: serde_json::json!({"observed_outcome":outcome,"reason":reason}),
            };
            match self.trace.emit(event(fact, vec![cause.clone()])).await {
                Ok(id) => cause = id,
                Err(error) => audit_errors.push(error.to_string()),
            }
            outcome = Outcome::denied(reason);
        }
        let fact = Fact::Completed {
            expression: expression.clone(),
            outcome: outcome.clone(),
            origin,
        };
        if let Err(error) = self.trace.emit(event(fact, vec![cause])).await {
            audit_errors.push(error.to_string());
        }
        Ok(Execution {
            operation_id,
            expression,
            outcome,
            audit_errors,
        })
    }
}

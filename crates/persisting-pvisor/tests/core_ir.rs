use async_trait::async_trait;
use persisting_control::{
    ir::*,
    trace::{Fact, Origin},
};
use persisting_pvisor::{
    core::{Admission, Backend, Engine, ExecutionContext},
    trace::{Journal, Trace},
};
use std::{collections::BTreeMap, sync::Arc};

const READ: &str = r#"fs.read("input", offset: 0, length: 5)"#;
fn context() -> Context {
    Context {
        revision: 1,
        principal: "demo".into(),
        policy: "demo-v1".into(),
        scope: vec!["run:test".into(), "attempt:1".into()],
        bindings: [("input", "source"), ("output", "destination")]
            .into_iter()
            .map(|(name, resource)| {
                (
                    name.into(),
                    Binding {
                        backend: "memory".into(),
                        resource: resource.into(),
                        generation: 1,
                        contract: 1,
                    },
                )
            })
            .collect(),
    }
}
#[derive(Default)]
struct Files {
    files: BTreeMap<String, Vec<u8>>,
    calls: Vec<Expression>,
    entered: Vec<String>,
    malformed: bool,
    blocked: bool,
    pending: Option<Arc<tokio::sync::Notify>>,
}
#[async_trait]
impl Backend for Files {
    fn name(&self) -> &str {
        "memory-model"
    }
    fn authorize(&self, context: &Context, expression: &Expression) -> Result<(), Failure> {
        let binding = context
            .bindings
            .get(expression.operation.file())
            .ok_or_else(|| Failure::Denied {
                reason: "unbound file".into(),
            })?;
        if self.blocked || binding.backend != "memory" || binding.generation != 1 {
            return Err(Failure::Denied {
                reason: "capability denied".into(),
            });
        }
        for layer in &expression.contexts {
            match layer {
                Layer::Vm { name } if name == "s1" => {}
                Layer::Remote { name } if name == "n1" => {}
                Layer::Overlay { name } if name == "workspace" => {}
                _ => {
                    return Err(Failure::Unsupported {
                        reason: "unsupported context combination".into(),
                    });
                }
            }
        }
        Ok(())
    }
    async fn execute(
        &mut self,
        context: &Context,
        expression: &Expression,
        _: &ExecutionContext,
    ) -> Outcome {
        self.calls.push(expression.clone());
        if let Some(started) = &self.pending {
            started.notify_one();
            std::future::pending::<()>().await;
        }
        let mut path = Vec::new();
        for layer in expression.contexts.iter().rev() {
            let name = match layer {
                Layer::Vm { name } | Layer::Remote { name } | Layer::Overlay { name } => name,
                _ => unreachable!(),
            };
            self.entered.push(name.clone());
            path.push(name.clone());
        }
        path.push(
            context.bindings[expression.operation.file()]
                .resource
                .clone(),
        );
        let resource = path.join("/");
        match &expression.operation {
            Operation::Read { offset, length, .. } => {
                let bytes = &self.files[&resource];
                let start = (*offset).min(bytes.len() as u64) as usize;
                let end = start
                    .saturating_add((*length).min(bytes.len() as u64) as usize)
                    .min(bytes.len());
                Outcome::success(Value::Bytes(if self.malformed {
                    vec![1; *length as usize + 1]
                } else {
                    bytes[start..end].to_vec()
                }))
            }
            Operation::Write { offset, data, .. } => {
                if *offset > 8192 || data.len() > 8192 - *offset as usize {
                    return Outcome::Error {
                        failure: Failure::Unsupported {
                            reason: "model file size".into(),
                        },
                    };
                }
                let file = self.files.entry(resource).or_default();
                let start = *offset as usize;
                if !data.is_empty() {
                    file.resize(file.len().max(start + data.len()), 0);
                    file[start..start + data.len()].copy_from_slice(data);
                }
                Outcome::success(Value::U64(data.len() as u64))
            }
        }
    }
}
struct Policy {
    admit: bool,
    rewrite: bool,
    deliver: bool,
}
impl Admission for Policy {
    fn admit(&self, _: &Context, _: &Expression) -> Result<(), String> {
        if self.admit {
            Ok(())
        } else {
            Err("request denied".into())
        }
    }
    fn rewrite(&self, _: &Context, _: &Rule, _: &Expression, _: &Expression) -> Result<(), String> {
        if self.rewrite {
            Ok(())
        } else {
            Err("rewrite denied".into())
        }
    }
    fn deliver(&self, _: &Context, _: &Expression, _: &Outcome) -> Result<(), String> {
        if self.deliver {
            Ok(())
        } else {
            Err("delivery denied".into())
        }
    }
}
fn engine() -> Engine<Files, Policy> {
    Engine {
        backend: Files {
            files: [
                ("source", "hello"),
                ("n1/source", "world"),
                ("n1/s1/source", "north"),
                ("s1/n1/source", "south"),
                ("workspace/source", "stage"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.as_bytes().to_vec()))
            .collect(),
            ..Files::default()
        },
        admission: Policy {
            admit: true,
            rewrite: true,
            deliver: true,
        },
        trace: Trace::new(Journal::memory(), "test"),
        passes: vec![],
    }
}
fn rule(rewrite: Rewrite) -> Rule {
    Rule {
        id: "test.rule".into(),
        version: 1,
        pattern: Pattern {
            operation: OpCode::Read,
            file: Some("input".into()),
            contexts: None,
        },
        rewrite,
    }
}

#[tokio::test]
async fn suffix_rewrite_keeps_request_and_records_exact_derivation_and_result() {
    let request: Expression = READ.parse().unwrap();
    let mut e = engine();
    e.passes = vec![vec![rule(Rewrite::Append {
        contexts: vec![Layer::Remote { name: "n1".into() }],
    })]];
    let run = e.run(&context(), &request).await.unwrap();
    assert_eq!(
        run.outcome,
        Outcome::success(Value::Bytes(b"world".to_vec()))
    );
    assert!(run.audit_errors.is_empty());
    assert!(request.contexts.is_empty());
    assert_eq!(request.operation, run.expression.operation);
    let records = e.trace.journal.records().unwrap();
    assert_eq!(records.len(), 5);
    assert!(
        matches!(&records[1].event.data, Fact::Requested { request: original } if original == &request)
    );
    assert!(
        matches!(&records[2].event.data, Fact::Rewritten { before, after, .. } if before == &request && after == &run.expression)
    );
    for pair in records.windows(2) {
        assert!(pair[1].event.caused_by.contains(&pair[0].event.id));
    }
    assert!(
        records[1..]
            .iter()
            .all(|r| r.event.operation.as_ref() == Some(&run.operation_id))
    );
    for record in records {
        record.event.validate().unwrap();
    }
}

#[tokio::test]
async fn outermost_context_runs_first_and_order_is_not_flattened() {
    for (suffix, expected, entered) in [
        (r#" |> vm("s1") |> remote("n1")"#, "north", vec!["n1", "s1"]),
        (r#" |> remote("n1") |> vm("s1")"#, "south", vec!["s1", "n1"]),
        (r#" |> overlay("workspace")"#, "stage", vec!["workspace"]),
    ] {
        let request: Expression = format!("{READ}{suffix}").parse().unwrap();
        let mut e = engine();
        let run = e.run(&context(), &request).await.unwrap();
        assert_eq!(
            run.outcome,
            Outcome::success(Value::Bytes(expected.as_bytes().to_vec()))
        );
        assert_eq!(e.backend.entered, entered);
        assert_eq!(e.backend.calls, [request]);
    }
}

#[tokio::test]
async fn outermost_mock_and_deny_short_circuit_the_entire_inner_computation() {
    for handler in [
        Layer::Mock {
            value: Value::Bytes(b"fake".to_vec()),
        },
        Layer::Deny {
            reason: "policy".into(),
        },
    ] {
        let mut e = engine();
        e.backend.blocked = true;
        e.passes = vec![vec![rule(Rewrite::Append {
            contexts: vec![Layer::Vm { name: "s1".into() }, handler.clone()],
        })]];
        let request: Expression = READ.parse().unwrap();
        let run = e.run(&context(), &request).await.unwrap();
        assert_eq!(run.outcome, handler.terminal().unwrap());
        assert!(e.backend.calls.is_empty());
        assert!(
            !e.trace
                .journal
                .records()
                .unwrap()
                .iter()
                .any(|r| matches!(r.event.data, Fact::Dispatched { .. }))
        );
        assert!(matches!(
            e.trace
                .journal
                .records()
                .unwrap()
                .last()
                .unwrap()
                .event
                .data,
            Fact::Completed {
                origin: Origin::Policy,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn phases_match_the_current_suffix_once_and_first_matching_rule_wins() {
    let mut first = rule(Rewrite::Append {
        contexts: vec![Layer::Vm { name: "s1".into() }],
    });
    first.pattern.contexts = Some(vec![]);
    let mut second = rule(Rewrite::Append {
        contexts: vec![Layer::Remote { name: "n1".into() }],
    });
    second.pattern.contexts = Some(vec![Layer::Vm { name: "s1".into() }]);
    let never = rule(Rewrite::SetContexts {
        contexts: vec![Layer::Deny {
            reason: "wrong rule".into(),
        }],
    });
    let mut e = engine();
    e.passes = vec![vec![first, never], vec![second]];
    let run = e.run(&context(), &READ.parse().unwrap()).await.unwrap();
    assert_eq!(
        run.outcome,
        Outcome::success(Value::Bytes(b"north".to_vec()))
    );
    assert_eq!(e.backend.calls.len(), 1);
    assert_eq!(run.expression.contexts.len(), 2);
}

#[tokio::test]
async fn explicit_operation_replacement_preserves_original_request_and_contract() {
    let original: Expression = READ.parse().unwrap();
    let mut replacement = original.clone();
    replacement.operation = Operation::Read {
        file: "input".into(),
        offset: 1,
        length: 3,
    };
    let mut e = engine();
    e.passes = vec![vec![rule(Rewrite::Replace {
        expression: replacement.clone(),
    })]];
    let run = e.run(&context(), &original).await.unwrap();
    assert_eq!(run.outcome, Outcome::success(Value::Bytes(b"ell".to_vec())));
    assert_eq!(run.expression, replacement);
    assert!(
        matches!(&e.trace.journal.records().unwrap()[1].event.data, Fact::Requested { request } if request == &original)
    );
    let mut larger = original.clone();
    larger.operation = Operation::Read {
        file: "input".into(),
        offset: 0,
        length: 10,
    };
    let mut e = engine();
    e.backend
        .files
        .insert("source".into(), b"0123456789".to_vec());
    e.passes = vec![vec![rule(Rewrite::Replace { expression: larger })]];
    assert!(matches!(
        e.run(&context(), &original).await.unwrap().outcome,
        Outcome::Error {
            failure: Failure::Unknown { .. }
        }
    ));
}

#[tokio::test]
async fn admission_final_authorization_and_invalid_rewrites_never_dispatch() {
    let request: Expression = READ.parse().unwrap();
    let mut e = engine();
    e.admission.admit = false;
    e.run(
        &context(),
        &format!("{READ} |> mock(bytes([1]))").parse().unwrap(),
    )
    .await
    .unwrap();
    assert!(e.backend.calls.is_empty());
    let mut e = engine();
    e.admission.rewrite = false;
    e.passes = vec![vec![rule(Rewrite::Append {
        contexts: vec![Layer::Remote { name: "n1".into() }],
    })]];
    assert!(matches!(
        e.run(&context(), &request).await.unwrap().outcome,
        Outcome::Error {
            failure: Failure::Denied { .. }
        }
    ));
    assert!(e.backend.calls.is_empty());
    let mut e = engine();
    e.backend.blocked = true;
    assert!(matches!(
        e.run(&context(), &request).await.unwrap().outcome,
        Outcome::Error {
            failure: Failure::Denied { .. }
        }
    ));
    assert!(e.backend.calls.is_empty());
    let mut e = engine();
    assert!(matches!(
        e.run(
            &context(),
            &format!("{READ} |> remote(\"unknown\")").parse().unwrap()
        )
        .await
        .unwrap()
        .outcome,
        Outcome::Error {
            failure: Failure::Unsupported { .. }
        }
    ));
    assert!(e.backend.calls.is_empty());
    let mut e = engine();
    e.passes = vec![vec![rule(Rewrite::Append {
        contexts: vec![Layer::Mock {
            value: Value::U64(1),
        }],
    })]];
    assert!(matches!(
        e.run(&context(), &request).await.unwrap().outcome,
        Outcome::Error {
            failure: Failure::Failed { .. }
        }
    ));
    assert!(e.backend.calls.is_empty());
}

#[tokio::test]
async fn bad_results_and_delivery_denial_keep_observed_effects() {
    let mut e = engine();
    e.backend.malformed = true;
    let run = e.run(&context(), &READ.parse().unwrap()).await.unwrap();
    assert!(matches!(
        run.outcome,
        Outcome::Error {
            failure: Failure::Unknown { .. }
        }
    ));
    assert_eq!(e.backend.calls.len(), 1);
    let mut e = engine();
    e.admission.deliver = false;
    e.run(&context(), &READ.parse().unwrap()).await.unwrap();
    assert!(e.trace.journal.records().unwrap().iter().any(|r| matches!(&r.event.data, Fact::Observation { name, payload, .. } if name == "delivery_denied" && payload["observed_outcome"]["status"] == "success")));
}

#[tokio::test]
async fn cancellation_leaves_dispatch_pending_and_audit_failure_keeps_known_result() {
    let mut e = engine();
    let started = Arc::new(tokio::sync::Notify::new());
    e.backend.pending = Some(started.clone());
    let context = context();
    let request: Expression = READ.parse().unwrap();
    let mut running = Box::pin(e.run(&context, &request));
    tokio::select! { _ = started.notified() => {}, _ = &mut running => panic!("pending backend returned") }
    drop(running);
    assert_eq!(e.backend.calls.len(), 1);
    assert!(
        !e.trace
            .journal
            .records()
            .unwrap()
            .iter()
            .any(|r| matches!(r.event.data, Fact::Completed { .. }))
    );
    let mut e = engine();
    let bytes = vec![1; 600_000];
    e.backend.files.insert("source".into(), bytes.clone());
    let large = Expression::new(Operation::Read {
        file: "input".into(),
        offset: 0,
        length: bytes.len() as u64,
    });
    let run = e.run(&context, &large).await.unwrap();
    assert_eq!(run.outcome, Outcome::success(Value::Bytes(bytes)));
    assert!(!run.audit_errors.is_empty());
    let mut e = engine();
    e.trace.producer.clear();
    assert!(e.run(&context, &request).await.is_err());
    assert!(e.backend.calls.is_empty());
}

#[tokio::test]
async fn serialized_request_and_rewrite_chain_reproduce_the_effective_expression() {
    let mut live = engine();
    live.passes = vec![vec![rule(Rewrite::Append {
        contexts: vec![Layer::Remote { name: "n1".into() }],
    })]];
    let execution = live.run(&context(), &READ.parse().unwrap()).await.unwrap();
    let records: Vec<persisting_control::trace::Record> = serde_json::from_str(
        &serde_json::to_string(&live.trace.journal.records().unwrap()).unwrap(),
    )
    .unwrap();
    let Fact::Requested { request } = &records[1].event.data else {
        unreachable!()
    };
    let mut replayed = request.clone();
    for record in &records {
        record.event.validate().unwrap();
        match &record.event.data {
            Fact::Rewritten {
                rule,
                before,
                after,
                ..
            } => {
                assert_eq!(&replayed, before);
                replayed = rule.apply(&replayed).unwrap();
                assert_eq!(&replayed, after);
            }
            Fact::Completed {
                expression,
                outcome,
                ..
            } => {
                assert_eq!(&replayed, expression);
                request.operation.check_outcome(outcome).unwrap();
                assert_eq!(outcome, &execution.outcome);
            }
            _ => {}
        }
    }
    assert_eq!(replayed, execution.expression);
}

#[tokio::test]
async fn write_and_short_read_obey_the_same_operation_contract() {
    let mut e = engine();
    let write: Expression = r#"fs.write("output", offset: 0, data: bytes([104,105]))"#
        .parse()
        .unwrap();
    assert_eq!(
        e.run(&context(), &write).await.unwrap().outcome,
        Outcome::success(Value::U64(2))
    );
    assert_eq!(e.backend.files["destination"], b"hi");
    for (offset, length, expected) in [
        (3, 10, b"lo".as_slice()),
        (100, 10, b"".as_slice()),
        (0, 0, b"".as_slice()),
    ] {
        let read = Expression::new(Operation::Read {
            file: "input".into(),
            offset,
            length,
        });
        assert_eq!(
            e.run(&context(), &read).await.unwrap().outcome,
            Outcome::success(Value::Bytes(expected.to_vec()))
        );
    }
}

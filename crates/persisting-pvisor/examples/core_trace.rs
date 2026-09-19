//! cargo run -p persisting-pvisor --example core_trace -- /tmp/read.trace.jsonl
//! A real file descriptor backend, scoped to a temporary file owned by this demo.
use async_trait::async_trait;
use persisting_control::ir::*;
use persisting_pvisor::{
    core::{Admission, Backend, Engine, ExecutionContext},
    trace::{Journal, Trace},
};
use std::{collections::BTreeMap, io::Write, os::unix::fs::FileExt, path::PathBuf};

struct FileBackend(std::fs::File);
#[async_trait]
impl Backend for FileBackend {
    fn name(&self) -> &str {
        "demo-file"
    }
    fn authorize(&self, context: &Context, expression: &Expression) -> Result<(), Failure> {
        let binding = context
            .bindings
            .get(expression.operation.file())
            .ok_or_else(|| Failure::Denied {
                reason: "unbound file".into(),
            })?;
        if context.principal == "demo"
            && binding.backend == "demo-file"
            && binding.resource == "owned-file"
            && binding.generation == 1
            && expression.contexts.is_empty()
        {
            Ok(())
        } else {
            Err(Failure::Unsupported {
                reason: "unsupported capability or context chain".into(),
            })
        }
    }
    async fn execute(
        &mut self,
        _: &Context,
        expression: &Expression,
        _: &ExecutionContext,
    ) -> Outcome {
        let Operation::Read { offset, length, .. } = expression.operation else {
            return Outcome::Error {
                failure: Failure::Unsupported {
                    reason: "read-only example".into(),
                },
            };
        };
        if length > 4096 {
            return Outcome::Error {
                failure: Failure::Unsupported {
                    reason: "demo reads at most 4096 bytes".into(),
                },
            };
        }
        let mut data = vec![0; length as usize];
        match self.0.read_at(&mut data, offset) {
            Ok(count) => {
                data.truncate(count);
                Outcome::success(Value::Bytes(data))
            }
            Err(error) => Outcome::Error {
                failure: Failure::Failed {
                    domain: "filesystem".into(),
                    code: "read_failed".into(),
                    effects: serde_json::json!({"kind":"none", "reason":error.to_string()}),
                },
            },
        }
    }
}
struct DemoPolicy;
impl Admission for DemoPolicy {
    fn admit(&self, context: &Context, _: &Expression) -> Result<(), String> {
        if context.policy == "demo-v1" {
            Ok(())
        } else {
            Err("unknown policy".into())
        }
    }
    fn rewrite(&self, _: &Context, _: &Rule, _: &Expression, _: &Expression) -> Result<(), String> {
        Ok(())
    }
    fn deliver(&self, _: &Context, _: &Expression, _: &Outcome) -> Result<(), String> {
        Ok(())
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: core_trace TRACE_PATH"))?,
    );
    let mut file = tempfile::tempfile()?;
    file.write_all(b"hello")?;
    let context = Context {
        revision: 1,
        principal: "demo".into(),
        policy: "demo-v1".into(),
        scope: vec!["run:demo".into()],
        bindings: BTreeMap::from([(
            "input".into(),
            Binding {
                backend: "demo-file".into(),
                resource: "owned-file".into(),
                generation: 1,
                contract: 1,
            },
        )]),
    };
    let request: Expression = include_str!("../../persisting-control/examples/read.pv").parse()?;
    let mut engine = Engine {
        backend: FileBackend(file),
        admission: DemoPolicy,
        trace: Trace::new(Journal::open(&path)?, "core-demo"),
        passes: vec![],
    };
    let real = engine.run(&context, &request).await?;
    assert_eq!(
        real.outcome,
        Outcome::success(Value::Bytes(b"hello".to_vec()))
    );
    engine.passes = vec![vec![Rule {
        id: "demo.mock".into(),
        version: 1,
        pattern: Pattern {
            operation: OpCode::Read,
            file: Some("input".into()),
            contexts: None,
        },
        rewrite: Rewrite::Append {
            contexts: vec![Layer::Mock {
                value: Value::Bytes(b"mock".to_vec()),
            }],
        },
    }]];
    let mock = engine.run(&context, &request).await?;
    assert_eq!(
        mock.outcome,
        Outcome::success(Value::Bytes(b"mock".to_vec()))
    );
    anyhow::ensure!(
        real.audit_errors.is_empty() && mock.audit_errors.is_empty(),
        "audit gaps"
    );
    for record in engine.trace.journal.records()? {
        println!("{} {}", record.position.offset, record.event.to_text()?);
    }
    println!("trace: {}", path.display());
    Ok(())
}

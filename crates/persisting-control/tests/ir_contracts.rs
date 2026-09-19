use persisting_control::{
    ir::*,
    trace::{Event, Fact, Granularity, Level, VERSION as EVENT_VERSION},
};
use proptest::prelude::*;

const READ: &str = r#"fs.read("input", offset: 0, length: 5)"#;
fn event(data: Fact) -> Event {
    Event {
        version: EVENT_VERSION,
        id: "fact".into(),
        trace_id: "trace".into(),
        producer: "test".into(),
        observed_at_unix_ms: 0,
        scope: vec!["run:test".into()],
        context: Some("context".into()),
        operation: Some("operation".into()),
        caused_by: vec![],
        level: Level::Info,
        granularity: Granularity::Operation,
        data,
    }
}
fn rule(rewrite: Rewrite) -> Rule {
    Rule {
        id: "route".into(),
        version: 1,
        pattern: Pattern {
            operation: OpCode::Read,
            file: Some("input".into()),
            contexts: None,
        },
        rewrite,
    }
}

#[test]
fn canonical_pipeline_and_json_preserve_the_operation_and_context_order() {
    for source in [
        READ.to_string(),
        format!("{READ} |> vm(\"s1\") |> remote(\"n1\")"),
        format!("{READ} |> remote(\"n1\") |> vm(\"s1\")"),
        format!("{READ} |> overlay(\"work\") |> mock(bytes([104,105]))"),
        format!("{READ} |> deny(\"policy\")"),
        r#"fs.write("output", data: bytes([0,255]), offset: 4) |> mock(2)"#.into(),
    ] {
        let expression: Expression = source.parse().unwrap();
        let text = expression.to_text().unwrap();
        assert_eq!(text.parse::<Expression>().unwrap(), expression);
        assert_eq!(text.parse::<Expression>().unwrap().to_text().unwrap(), text);
        let decoded: Expression =
            serde_json::from_str(&serde_json::to_string(&expression).unwrap()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, expression);
    }
    let forward: Expression = format!("{READ} |> vm(\"s1\") |> remote(\"n1\")")
        .parse()
        .unwrap();
    let reverse: Expression = format!("{READ} |> remote(\"n1\") |> vm(\"s1\")")
        .parse()
        .unwrap();
    assert_ne!(forward, reverse);
    assert_eq!(forward.operation, reverse.operation);
}

#[test]
fn invalid_contracts_and_ambiguous_handlers_are_rejected() {
    for source in [
        r#"fs.read("input", offset: 0, length: 1, length: 2)"#,
        r#"fs.read("input", offset: 0, length: 1, other: 2)"#,
        r#"fs.read("input", offset: 0)"#,
        r#"fs.read("input", offset: 18446744073709551615, length: 1)"#,
        r#"fs.read("input", offset: -1, length: 1)"#,
        r#"fs.read("input", offset: 0, length: 1) |> maybe()"#,
        r#"fs.read("input", offset: 0, length: 1) |> mock(1)"#,
        r#"fs.read("input", offset: 0, length: 1) |> mock(bytes([1,2]))"#,
        r#"fs.read("input", offset: 0, length: 1) |> deny("no") |> vm("s1")"#,
        r#"fs.read("input", offset: 0, length: 1) |> mock(bytes([1])) |> deny("no")"#,
        r#"fs.read("input", offset: 0, length: 1) |> vm("")"#,
        r#"fs.write("output", offset: 0, data: bytes([256]))"#,
        r#"fs.read@2("input", offset: 0, length: 1)"#,
        "pvisor 2; fn old() -> unit { unit }",
        r#"fs.read("input", offset: 0, length: 1); trailing"#,
    ] {
        assert!(
            source
                .parse::<Expression>()
                .unwrap_err()
                .to_string()
                .starts_with("IR "),
            "{source}"
        );
    }
    let too_many = format!("{READ}{}", " |> vm(\"s1\")".repeat(MAX_CONTEXTS + 1));
    assert!(too_many.parse::<Expression>().is_err());
    let mut expression: Expression = READ.parse().unwrap();
    expression.version = 2;
    let decoded: Expression =
        serde_json::from_str(&serde_json::to_string(&expression).unwrap()).unwrap();
    assert!(decoded.validate().is_err());
}

#[test]
fn suffix_rewrites_preserve_the_original_and_events_verify_the_derivation() {
    let before: Expression = READ.parse().unwrap();
    let rule = rule(Rewrite::Append {
        contexts: vec![Layer::Vm { name: "s1".into() }],
    });
    let after = rule.apply(&before).unwrap();
    assert!(before.contexts.is_empty());
    assert_eq!(before.operation, after.operation);
    let fact = event(Fact::Rewritten {
        rule: rule.clone(),
        pass: 0,
        before: before.clone(),
        after: after.clone(),
    });
    fact.validate().unwrap();
    let text = fact.to_text().unwrap();
    assert!(text.contains("rewritten") && text.contains("|> vm("));
    let decoded: Event = serde_json::from_str(&serde_json::to_string(&fact).unwrap()).unwrap();
    assert_eq!(fact, decoded);
    let mut forged = after;
    forged.operation = Operation::Read {
        file: "secret".into(),
        offset: 0,
        length: 5,
    };
    assert!(
        event(Fact::Rewritten {
            rule,
            pass: 0,
            before,
            after: forged
        })
        .validate()
        .is_err()
    );
    let bad = event(Fact::Completed {
        expression: READ.parse().unwrap(),
        outcome: Outcome::success(Value::U64(5)),
        origin: persisting_control::trace::Origin::Backend,
    });
    assert!(bad.validate().is_err());
}

#[test]
fn explicit_replacement_and_set_contexts_have_distinct_meanings() {
    let original: Expression = format!("{READ} |> vm(\"s1\")").parse().unwrap();
    let changed = rule(Rewrite::SetContexts {
        contexts: vec![Layer::Remote { name: "n1".into() }],
    })
    .apply(&original)
    .unwrap();
    assert_eq!(original.operation, changed.operation);
    assert_eq!(changed.contexts.len(), 1);
    let replacement: Expression = r#"fs.read("other", offset: 1, length: 4)"#.parse().unwrap();
    assert_eq!(
        rule(Rewrite::Replace {
            expression: replacement.clone()
        })
        .apply(&original)
        .unwrap(),
        replacement
    );
    let incompatible: Expression =
        r#"fs.write("other", offset: 0, data: bytes([]))"#.parse().unwrap();
    assert!(
        rule(Rewrite::Replace {
            expression: incompatible
        })
        .validate()
        .is_err()
    );
}

proptest! {
    #[test]
    fn unicode_paths_and_bytes_roundtrip(path in ".{1,100}", data in prop::collection::vec(any::<u8>(), 0..100), offset in 0u64..10000) {
        let expression = Expression::new(Operation::Write { file: path, offset, data });
        prop_assert_eq!(expression.to_text().unwrap().parse::<Expression>().unwrap(), expression);
    }
    #[test]
    fn arbitrary_text_never_panics(source in ".{0,2000}") { let _ = source.parse::<Expression>(); }
    #[test]
    fn context_append_is_associative_and_does_not_mutate_the_operation(a in prop::collection::vec("[a-z]{1,8}", 0..8), b in prop::collection::vec("[a-z]{1,8}", 0..8)) {
        let before: Expression = READ.parse().unwrap();
        let a: Vec<_> = a.into_iter().map(|name| Layer::Vm { name }).collect();
        let b: Vec<_> = b.into_iter().map(|name| Layer::Remote { name }).collect();
        let once = rule(Rewrite::Append { contexts: a.clone() }).apply(&before).unwrap();
        let twice = rule(Rewrite::Append { contexts: b.clone() }).apply(&once).unwrap();
        let combined = rule(Rewrite::Append { contexts: a.into_iter().chain(b).collect() }).apply(&before).unwrap();
        prop_assert_eq!(twice, combined); prop_assert!(before.contexts.is_empty());
    }
}

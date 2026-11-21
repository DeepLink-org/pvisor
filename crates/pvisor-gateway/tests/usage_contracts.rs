use proptest::prelude::*;
use pvisor_gateway::{
    conversion::{ProtocolBridge, StreamTranslator, translate_response_for_bridge},
    protocol::ProtocolKind,
    provider::ProviderKind,
    session::index::SessionIndexStore,
    understanding::understand_response_value,
    usage::{TokenUsage, extract_usage_from_response, extract_usage_from_sse},
};
use serde_json::{Value, json};

const FIELDS: [&str; 6] = [
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
];

fn usage(counts: [u64; 6]) -> TokenUsage {
    let [
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    ] = counts;
    TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    }
}

fn counts(usage: &TokenUsage) -> [u64; 6] {
    [
        usage.input_tokens,
        usage.output_tokens,
        usage.total_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        usage.reasoning_tokens,
    ]
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

#[rstest::rstest]
#[case::zero(0, 0, Some(0))]
#[case::exact_max(u64::MAX, 0, Some(u64::MAX))]
#[case::exact_boundary(u64::MAX - 1, 1, Some(u64::MAX))]
#[case::one_over(u64::MAX, 1, None)]
#[case::twice_max(u64::MAX, u64::MAX, None)]
fn inferred_total_boundaries(
    #[case] input: u64,
    #[case] output: u64,
    #[case] expected: Option<u64>,
) {
    for total in [Value::Null, json!(0)] {
        let mut body = json!({"usage": {"input_tokens": input, "output_tokens": output}});
        if !total.is_null() {
            body["usage"]["total_tokens"] = total;
        }
        let result = extract_usage_from_response(&body);
        if let Some(total) = expected {
            let result = result.unwrap();
            assert_eq!(
                (
                    result.input_tokens,
                    result.output_tokens,
                    result.total_tokens
                ),
                (input, output, total)
            );
        } else {
            assert!(result.unwrap_err().to_string().contains("total_tokens"));
        }
    }
}

#[rstest::rstest]
fn explicit_totals_and_missing_usage_keep_existing_policy() {
    let result = extract_usage_from_response(&json!({
        "usage": {"input_tokens": u64::MAX, "output_tokens": 1, "total_tokens": 17}
    }))
    .unwrap();
    assert_eq!(result.total_tokens, 17);
    assert_eq!(
        extract_usage_from_response(&json!({})).unwrap(),
        TokenUsage::default()
    );
    assert_eq!(
        extract_usage_from_sse("data: invalid\n\ndata: [DONE]\n\n").unwrap(),
        TokenUsage::default()
    );
}

#[rstest::rstest]
#[case::input(0)]
#[case::output(1)]
#[case::total(2)]
#[case::cache_read(3)]
#[case::cache_write(4)]
#[case::reasoning(5)]
fn merge_and_session_overflow_are_atomic(#[case] field: usize) {
    let mut initial = [1; 6];
    initial[field] = u64::MAX;
    let mut current = usage(initial);
    let delta = usage([1; 6]);
    assert!(
        current
            .merge(&delta)
            .unwrap_err()
            .to_string()
            .contains(FIELDS[field])
    );
    assert_eq!(current, usage(initial));

    let dir = tempfile::tempdir().unwrap();
    let store = SessionIndexStore::open(dir.path()).unwrap();
    let handle = store.clone_handle();
    handle
        .record_request("agent", "session", ProviderKind::OpenAi, "chat", "old")
        .unwrap();
    handle
        .record_response(
            "agent",
            "session",
            ProviderKind::OpenAi,
            "old",
            &current,
            0.25,
        )
        .unwrap();
    handle
        .record_request("agent", "session", ProviderKind::OpenAi, "chat", "old")
        .unwrap();
    handle.flush_if_dirty().unwrap();
    let path = dir.path().join(".capture/sessions.json");
    let before = std::fs::read(&path).unwrap();
    assert!(
        handle
            .record_response(
                "agent",
                "session",
                ProviderKind::Anthropic,
                "new",
                &delta,
                1.0
            )
            .is_err()
    );
    handle.flush_if_dirty().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // Force a write to check memory as well as the clean on-disk snapshot.
    store.save().unwrap();
    let saved = SessionIndexStore::load(dir.path()).unwrap();
    let before: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        serde_json::to_value(&saved.sessions).unwrap(),
        before["sessions"]
    );
    let mut valid = [1; 6];
    valid[field] = 0;
    handle
        .record_response(
            "agent",
            "session",
            ProviderKind::OpenAi,
            "valid",
            &usage(valid),
            0.5,
        )
        .unwrap();
    handle.flush_if_dirty().unwrap();
    let saved = SessionIndexStore::load(dir.path()).unwrap();
    assert_eq!(
        counts(&saved.sessions[0].usage),
        std::array::from_fn(|i| if i == field { u64::MAX } else { 2 })
    );
    assert_eq!(saved.sessions[0].estimated_cost_usd, 0.75);
    assert_eq!(saved.sessions[0].model, "valid");
    assert!(!saved.sessions[0].active);
}

#[rstest::rstest]
#[case::top_level(false)]
#[case::nested_message(true)]
fn sse_rejects_aggregate_overflow(#[case] nested: bool) {
    let event = json!({"usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": u64::MAX}});
    let event = if nested {
        json!({"message": event})
    } else {
        event
    };
    let raw = frame(event) + &frame(json!({"usage": {"total_tokens": 1}}));
    assert!(
        extract_usage_from_sse(&raw)
            .unwrap_err()
            .to_string()
            .contains("total_tokens")
    );
}

fn overflowing_body(protocol: ProtocolKind) -> Value {
    match protocol {
        ProtocolKind::Gemini => {
            json!({"usageMetadata": {"promptTokenCount": u64::MAX, "candidatesTokenCount": 1}})
        }
        _ => json!({"usage": {"input_tokens": u64::MAX, "output_tokens": 1}}),
    }
}

#[rstest::rstest]
#[case::chat(ProtocolKind::ChatCompletions)]
#[case::messages(ProtocolKind::Messages)]
#[case::responses(ProtocolKind::Responses)]
#[case::gemini(ProtocolKind::Gemini)]
fn production_response_paths_reject_overflow(#[case] protocol: ProtocolKind) {
    let body = overflowing_body(protocol);
    assert!(
        understand_response_value(protocol, &body)
            .unwrap_err()
            .to_string()
            .contains("total_tokens")
    );
    let raw = bytes::Bytes::from(serde_json::to_vec(&body).unwrap());
    assert!(
        translate_response_for_bridge(ProtocolBridge::Passthrough, &raw, protocol, "client")
            .is_err()
    );
}

fn overflowing_stream(protocol: ProtocolKind) -> String {
    match protocol {
        ProtocolKind::Messages => {
            frame(json!({"type": "message_start", "message": {
                "usage": {"input_tokens": u64::MAX, "output_tokens": 0}
            }})) + &frame(json!({"type": "message_delta", "usage": {"output_tokens": 1}}))
                + &frame(
                    json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 0}}),
                )
                + &frame(json!({"type": "message_stop"}))
        }
        ProtocolKind::Responses => {
            frame(json!({"type": "response.completed", "response": overflowing_body(protocol)}))
        }
        ProtocolKind::Gemini => {
            let mut body = overflowing_body(protocol);
            body["candidates"] = json!([{"index": 0, "finishReason": "STOP"}]);
            frame(body)
        }
        _ => {
            let mut body = overflowing_body(protocol);
            body["choices"] = json!([{"index": 0, "delta": {}, "finish_reason": "stop"}]);
            frame(body) + "data: [DONE]\n\n"
        }
    }
}

#[rstest::rstest]
#[case::chat(ProtocolKind::ChatCompletions)]
#[case::messages(ProtocolKind::Messages)]
#[case::responses(ProtocolKind::Responses)]
#[case::gemini(ProtocolKind::Gemini)]
fn overflow_passthrough_preserves_bytes_but_never_completes(#[case] protocol: ProtocolKind) {
    let raw = overflowing_stream(protocol);
    for split in 0..=raw.len() {
        let mut stream =
            StreamTranslator::new(ProtocolBridge::Passthrough, protocol, "client").unwrap();
        let mut output = stream
            .push_chunk(&raw.as_bytes()[..split])
            .unwrap()
            .to_vec();
        output.extend_from_slice(&stream.push_chunk(&raw.as_bytes()[split..]).unwrap());
        assert_eq!(output, raw.as_bytes());
        assert_eq!(stream.upstream_snapshot(), raw.as_bytes());
        assert!(stream.finish_stream().is_err());
        assert!(stream.finish_stream().is_err());
    }
}

#[rstest::rstest]
#[case::chat_messages(
    ProtocolBridge::MessagesToCompletions,
    ProtocolKind::Messages,
    ProtocolKind::ChatCompletions
)]
#[case::chat_responses(
    ProtocolBridge::ResponsesToCompletions,
    ProtocolKind::Responses,
    ProtocolKind::ChatCompletions
)]
#[case::gemini_chat(
    ProtocolBridge::CompletionsToGemini,
    ProtocolKind::ChatCompletions,
    ProtocolKind::Gemini
)]
#[case::gemini_messages(
    ProtocolBridge::MessagesToGemini,
    ProtocolKind::Messages,
    ProtocolKind::Gemini
)]
#[case::gemini_responses(
    ProtocolBridge::ResponsesToGemini,
    ProtocolKind::Responses,
    ProtocolKind::Gemini
)]
fn translated_stream_overflow_is_terminal(
    #[case] bridge: ProtocolBridge,
    #[case] client: ProtocolKind,
    #[case] upstream: ProtocolKind,
) {
    let body = bytes::Bytes::from(serde_json::to_vec(&overflowing_body(upstream)).unwrap());
    assert!(translate_response_for_bridge(bridge, &body, client, "client").is_err());
    let raw = overflowing_stream(upstream);
    let mut stream = StreamTranslator::new(bridge, client, "client").unwrap();
    assert!(
        stream
            .push_chunk(raw.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("total_tokens")
    );
    // A later valid terminal marker cannot turn rejected usage into a success.
    let _ = stream.push_chunk(b"data: [DONE]\n\n");
    assert!(stream.finish_stream().is_err());
}

proptest! {
    #[test]
    fn generated_merge_sequences_match_wide_arithmetic(
        initial in any::<[u64; 6]>(),
        deltas in prop::collection::vec(any::<[u64; 6]>(), 0..24),
    ) {
        let mut current = usage(initial);
        for delta in deltas {
            let before = current.clone();
            let expected: [u128; 6] = std::array::from_fn(|i| u128::from(counts(&before)[i]) + u128::from(delta[i]));
            let result = current.merge(&usage(delta));
            if expected.iter().all(|sum| *sum <= u128::from(u64::MAX)) {
                prop_assert!(result.is_ok());
                prop_assert_eq!(counts(&current).map(u128::from), expected);
            } else {
                prop_assert!(result.is_err());
                prop_assert_eq!(&current, &before);
            }
        }
    }

    #[test]
    fn generated_inferred_totals_match_wide_arithmetic(input in any::<u64>(), output in any::<u64>()) {
        let sum = u128::from(input) + u128::from(output);
        let result = extract_usage_from_response(&json!({"usage": {"prompt_tokens": input, "completion_tokens": output}}));
        if sum > u128::from(u64::MAX) {
            prop_assert!(result.is_err());
        } else {
            prop_assert_eq!(u128::from(result.unwrap().total_tokens), sum);
        }
    }
}

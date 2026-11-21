use bytes::Bytes;
use proptest::prelude::*;
use pvisor_gateway::{
    conversion::{
        ProtocolBridge, StreamTranslator, translate_request_for_bridge,
        translate_response_for_bridge,
    },
    protocol::ProtocolKind,
    understanding::understand_request_value,
};
use serde_json::{Value, json};

fn request(protocol: ProtocolKind) -> (ProtocolBridge, Value) {
    let schema = json!({"type": "object", "properties": {"value": {"type": "string"}}});
    let (bridge, mut body) = match protocol {
        ProtocolKind::ChatCompletions => (
            ProtocolBridge::CompletionsToGemini,
            json!({
                "messages": [
                    {"role": "system", "content": "system"},
                    {"role": "user", "content": "first"}, {"role": "user", "content": "second"}
                ],
                "tools": [{"type": "function", "function": {"name": "lookup", "parameters": schema}}],
                "tool_choice": "required", "max_tokens": 20
            }),
        ),
        ProtocolKind::Messages => (
            ProtocolBridge::MessagesToGemini,
            json!({
                "system": "system",
                "messages": [{"role": "user", "content": "first"}, {"role": "user", "content": "second"}],
                "tools": [{"name": "lookup", "input_schema": schema}],
                "tool_choice": {"type": "any"}, "max_tokens": 20
            }),
        ),
        ProtocolKind::Responses => (
            ProtocolBridge::ResponsesToGemini,
            json!({
                "instructions": "system",
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "first"}]},
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "second"}]}
                ],
                "tools": [{"type": "function", "name": "lookup", "parameters": schema}],
                "tool_choice": "required", "max_output_tokens": 20
            }),
        ),
        _ => unreachable!(),
    };
    body["model"] = json!("client");
    body["temperature"] = json!(0.25);
    body["labels"] = json!({"test": "retained"});
    body["safetySettings"] = json!([]);
    (bridge, body)
}

#[rstest::rstest]
#[case::absent(None)]
#[case::null(Some(Value::Null))]
#[case::empty(Some(json!("")))]
#[case::cached(Some(json!("projects/test/cachedContents/example")))]
fn gemini_cached_content_preserves_other_fields(
    #[case] cached: Option<Value>,
    #[values(
        ProtocolKind::ChatCompletions,
        ProtocolKind::Messages,
        ProtocolKind::Responses
    )]
    protocol: ProtocolKind,
) {
    let (bridge, mut body) = request(protocol);
    if let Some(value) = &cached {
        body["cachedContent"] = value.clone();
    }
    let semantic = understand_request_value(protocol, &body).unwrap();
    let raw = translate_request_for_bridge(bridge, &semantic, "gemini-2.5-pro", None).unwrap();
    let wire: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        wire["contents"],
        json!([{"role": "user", "parts": [{"text": "first"}, {"text": "second"}]}])
    );
    assert_eq!(wire["generationConfig"]["maxOutputTokens"], 20);
    assert_eq!(wire["generationConfig"]["temperature"], 0.25);
    assert_eq!(wire["labels"], body["labels"]);
    assert_eq!(wire["safetySettings"], body["safetySettings"]);
    if let Some(value) = cached.filter(|value| !value.is_null()) {
        assert_eq!(wire["cachedContent"], value);
        for field in ["systemInstruction", "tools", "toolConfig"] {
            assert!(wire.get(field).is_none(), "{field}: {wire}");
        }
    } else {
        assert!(wire.get("cachedContent").is_none());
        assert_eq!(wire["systemInstruction"]["parts"][0]["text"], "system");
        assert_eq!(
            wire["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
        assert_eq!(wire["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }
}

#[rstest::rstest]
#[case::missing(None)]
#[case::zero(Some(0))]
#[case::nonzero(Some(3))]
#[case::maximum(Some(u64::MAX))]
fn optional_usage_presence_and_capture_totals_survive_translation(
    #[case] count: Option<u64>,
    #[values(false, true)] gemini: bool,
) {
    let (bridge, protocol, mut body) = if gemini {
        (
            ProtocolBridge::CompletionsToGemini,
            ProtocolKind::ChatCompletions,
            json!({
                "responseId": "fixed", "candidates": [{
                    "index": 0, "content": {"role": "model", "parts": [{"text": "ok"}]}, "finishReason": "STOP"
                }],
                "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2, "totalTokenCount": 12}
            }),
        )
    } else {
        (
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
            json!({
                "id": "fixed", "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
                "service_tier": "priority"
            }),
        )
    };
    if let Some(count) = count {
        if gemini {
            body["usageMetadata"]["cachedContentTokenCount"] = json!(count);
            body["usageMetadata"]["thoughtsTokenCount"] = json!(count);
        } else {
            body["usage"]["prompt_tokens_details"] =
                json!({"cached_tokens": count, "cache_write_tokens": count});
            body["usage"]["completion_tokens_details"] = json!({"reasoning_tokens": count});
        }
    }
    let translated = translate_response_for_bridge(
        bridge,
        &Bytes::from(serde_json::to_vec(&body).unwrap()),
        protocol,
        "client",
    )
    .unwrap();
    let wire: Value = serde_json::from_slice(&translated.body).unwrap();
    let mut expected = if gemini {
        json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12})
    } else {
        let input = match count {
            None | Some(0) => 10,
            Some(3) => 4,
            _ => 0,
        };
        json!({"input_tokens": input, "output_tokens": 2, "service_tier": "priority"})
    };
    if let Some(count) = count {
        if gemini {
            expected["prompt_tokens_details"] = json!({"cached_tokens": count});
            expected["completion_tokens_details"] = json!({"reasoning_tokens": count});
        } else {
            expected["cache_read_input_tokens"] = json!(count);
            expected["cache_creation_input_tokens"] = json!(count);
        }
    }
    assert_eq!(wire["usage"], expected);
    assert_eq!(
        translated
            .semantic
            .response
            .usage
            .as_ref()
            .unwrap()
            .input_tokens,
        10
    );
}

#[rstest::rstest]
fn streamed_chat_usage_keeps_the_shared_usage_wire_shape() {
    let frame = json!({
        "candidates": [{"index": 0, "content": {"role": "model", "parts": [{"text": "ok"}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2, "totalTokenCount": 12, "cachedContentTokenCount": 0, "thoughtsTokenCount": 0}
    });
    let mut stream = StreamTranslator::new(
        ProtocolBridge::CompletionsToGemini,
        ProtocolKind::ChatCompletions,
        "client",
    )
    .unwrap();
    let mut bytes = stream
        .push_chunk(format!("data: {frame}\n\n").as_bytes())
        .unwrap()
        .to_vec();
    bytes.extend_from_slice(&stream.finish_stream().unwrap());
    let events = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let usage = events.iter().find_map(|event| event.get("usage")).unwrap();
    assert_eq!(
        usage,
        &json!({
            "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12,
            "prompt_tokens_details": {"cached_tokens": 0},
            "completion_tokens_details": {"reasoning_tokens": 0}
        })
    );
}

proptest! {
    #[test]
    fn generated_gemini_tool_results_keep_call_order_and_text_boundaries(
        count in 1usize..5,
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        let calls = (0..count).map(|index| json!({
            "id": format!("call-{index}"), "type": "function",
            "function": {"name": "lookup", "arguments": json!({"ordinal": index}).to_string()}
        })).collect::<Vec<_>>();
        let mut messages = vec![
            json!({"role": "user", "content": "query"}),
            json!({"role": "assistant", "tool_calls": calls}),
        ];
        for index in (0..count).rev() {
            messages.push(json!({
                "role": "tool", "tool_call_id": format!("call-{index}"),
                "content": format!("{index}:{payload}")
            }));
        }
        messages.extend([
            json!({"role": "user", "content": "followup"}),
            json!({"role": "user", "content": "last"}),
        ]);
        let semantic = understand_request_value(
            ProtocolKind::ChatCompletions, &json!({"model": "client", "messages": messages}),
        ).unwrap();
        let raw = translate_request_for_bridge(
            ProtocolBridge::CompletionsToGemini, &semantic, "gemini-2.5-pro", None,
        ).unwrap();
        let wire: Value = serde_json::from_slice(&raw).unwrap();
        let contents = wire["contents"].as_array().unwrap();
        prop_assert_eq!(contents.len(), 4);
        let results = contents[2]["parts"].as_array().unwrap();
        prop_assert_eq!(results.len(), count);
        for (index, result) in results.iter().enumerate() {
            prop_assert_eq!(&result["functionResponse"], &json!({
                "name": "lookup", "response": {"content": format!("{index}:{payload}")}
            }));
            prop_assert_eq!(&contents[1]["parts"][index]["functionCall"]["args"], &json!({"ordinal": index}));
        }
        prop_assert_eq!(&contents[3], &json!({"role": "user", "parts": [{"text": "followup"}, {"text": "last"}]}));
    }
}

//! Contract regressions retained from the retired wire-to-wire converters.

use bytes::Bytes;
use pvisor_gateway::conversion::{
    ProtocolBridge, StreamTranslator, translate_request_for_bridge, translate_response_for_bridge,
};
use pvisor_gateway::protocol::ProtocolKind;
use pvisor_gateway::understanding::{understand_request, understand_request_value};
use serde_json::{Value, json};

#[test]
fn cached_input_counts_match_between_complete_and_streamed_messages() {
    for (input, read, written, expected) in [(100, 20, 10, 70), (100, 0, 0, 100), (5, 8, 3, 0)] {
        let usage = json!({
            "prompt_tokens":input,
            "completion_tokens":5,
            "prompt_tokens_details":{"cached_tokens":read,"cache_write_tokens":written}
        });
        let complete = json!({
            "id":"completion",
            "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":usage
        });
        let complete = translate_response_for_bridge(
            ProtocolBridge::MessagesToCompletions,
            &Bytes::from(serde_json::to_vec(&complete).unwrap()),
            ProtocolKind::Messages,
            "client",
        )
        .unwrap();
        let complete: Value = serde_json::from_slice(&complete.body).unwrap();
        let chunk = json!({
            "id":"completion",
            "choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}],
            "usage":usage
        });
        let mut translator = StreamTranslator::new(
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
            "client",
        )
        .unwrap();
        let mut output = translator
            .push_chunk(format!("data: {chunk}\n\n").as_bytes())
            .unwrap()
            .to_vec();
        output.extend_from_slice(&translator.finish_stream().unwrap());
        let streamed = String::from_utf8(output)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|event| event["type"] == "message_delta")
            .unwrap();
        for response in [&complete, &streamed] {
            assert_eq!(response["usage"]["input_tokens"], expected);
            assert_eq!(response["usage"]["cache_read_input_tokens"], read);
            assert_eq!(
                response["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(0),
                written
            );
        }
        // Capture metrics retain provider totals; only client wire rendering changes.
        assert_eq!(translator.metrics().usage.input_tokens, input);
    }
}

#[test]
fn recognizes_tool_call_types() {
    for kind in ["function_call", "custom_tool_call"] {
        let body = json!({"input":[
            {"type":kind,"call_id":"call_1","name":"shell","arguments":"{\"command\":\"ls\"}"},
            {"type":format!("{kind}_output"),"call_id":"call_1","output":"ok"}
        ]});
        let parsed = understand_request_value(ProtocolKind::Responses, &body).unwrap();
        let out = translate_request_for_bridge(
            ProtocolBridge::ResponsesToCompletions,
            &parsed,
            "upstream",
            None,
        )
        .unwrap();
        let wire: Value = serde_json::from_slice(&out).unwrap();
        let messages = wire["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        assert_eq!(messages[1]["role"], "tool");
        assert!(messages[1].get("tool_calls").is_none());
        assert_eq!(messages[1]["tool_call_id"], "call_1");
        assert_eq!(messages[1]["content"], "ok");
    }
}

#[test]
fn codex_like_request_to_completions() {
    let body = Bytes::from_static(include_bytes!(
        "fixtures/local/requests/responses/codex_basic.json"
    ));
    let parsed = understand_request(ProtocolKind::Responses, &body).unwrap();
    let out = translate_request_for_bridge(
        ProtocolBridge::ResponsesToCompletions,
        &parsed.semantic,
        "deepseek-chat",
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["model"], "deepseek-chat");
    assert_eq!(v["stream"], true);
    let msgs = v["messages"].as_array().unwrap();
    assert!(msgs.len() >= 2);
    assert_eq!(msgs[0]["role"], "user");
    assert!(msgs[0]["content"].as_str().unwrap().contains("permissions"));
    assert!(
        msgs.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("hi")
    );
}

#[test]
fn tool_roundtrip_request_to_completions() {
    let body = Bytes::from_static(include_bytes!(
        "fixtures/local/requests/responses/codex_tool_roundtrip.json"
    ));
    let parsed = understand_request(ProtocolKind::Responses, &body).unwrap();
    let out = translate_request_for_bridge(
        ProtocolBridge::ResponsesToCompletions,
        &parsed.semantic,
        "deepseek-chat",
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(v.get("tools").is_some());
    let tool = &v["tools"][0];
    assert!(tool.get("function").is_some());
    assert_eq!(tool["function"]["name"], "shell");
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "shell");
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["tool_call_id"], "call_abc123");
    assert!(msgs[2]["content"].as_str().unwrap().contains("Cargo.toml"));
}

#[test]
fn completions_tool_call_to_responses_output() {
    let body = Bytes::from_static(include_bytes!(
        "fixtures/local/response/completions/tool_call.json"
    ));
    let out = translate_response_for_bridge(
        ProtocolBridge::ResponsesToCompletions,
        &body,
        ProtocolKind::Responses,
        "gpt-5.5",
    )
    .unwrap()
    .body;
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["output"][0]["type"], "function_call");
    assert_eq!(v["output"][0]["call_id"], "call_abc123");
    assert_eq!(v["output"][0]["name"], "shell");
    assert!(
        v["output"][0]["arguments"]
            .as_str()
            .unwrap()
            .contains("ls -la")
    );
}

#[test]
fn batches_consecutive_function_calls_into_one_assistant_message() {
    let body = json!({
        "model": "gpt-5.5",
        "stream": true,
        "input": [
            {"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]},
            {"type":"function_call","call_id":"call_a","name":"shell","arguments":"{}"},
            {"type":"function_call","call_id":"call_b","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"call_a","output":"ok"},
            {"type":"function_call_output","call_id":"call_b","output":"file"}
        ]
    });
    let parsed = understand_request_value(ProtocolKind::Responses, &body).unwrap();
    let out = translate_request_for_bridge(
        ProtocolBridge::ResponsesToCompletions,
        &parsed,
        "deepseek-chat",
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["tool_call_id"], "call_a");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_b");
}

#[test]
fn parallel_same_name_tool_results_restore_call_order() {
    let body = json!({
        "model":"gemini-2.5-pro",
        "messages":[
            {"role":"user","content":"compare"},
            {"role":"assistant","tool_calls":[
                {"id":"call_a","type":"function","function":{"name":"lookup","arguments":"{\"id\":\"a\"}"}},
                {"id":"call_b","type":"function","function":{"name":"lookup","arguments":"{\"id\":\"b\"}"}}
            ]},
            {"role":"tool","tool_call_id":"call_b","content":"result-b"},
            {"role":"tool","tool_call_id":"call_a","content":"result-a"}
        ]
    });
    let parsed = understand_request_value(ProtocolKind::ChatCompletions, &body).unwrap();
    let translated = translate_request_for_bridge(
        ProtocolBridge::CompletionsToGemini,
        &parsed,
        "gemini-2.5-pro",
        None,
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&translated).unwrap();
    let responses = value["contents"][2]["parts"].as_array().unwrap();
    assert_eq!(
        responses[0]["functionResponse"]["response"]["content"],
        "result-a"
    );
    assert_eq!(
        responses[1]["functionResponse"]["response"]["content"],
        "result-b"
    );
    assert!(
        responses
            .iter()
            .all(|response| response["functionResponse"].get("id").is_none())
    );
}

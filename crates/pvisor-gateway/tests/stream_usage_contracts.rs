use proptest::prelude::*;
use pvisor_gateway::{
    conversion::{ProtocolBridge, StreamTranslator},
    llm::LlmContentPart,
    protocol::ProtocolKind,
};
use serde_json::{Value, json};

fn frame(value: Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

fn frames(input: u64, first: u64, last: u64, text: &str) -> Vec<Vec<u8>> {
    vec![
        frame(json!({"type": "message_start", "message": {
            "id": "fixed", "model": "provider", "role": "assistant", "content": [],
            "usage": {"input_tokens": input, "output_tokens": 0,
                      "cache_read_input_tokens": 20, "cache_creation_input_tokens": 10}
        }})),
        frame(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        frame(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        frame(json!({"type": "content_block_stop", "index": 0})),
        frame(json!({"type": "message_delta", "usage": {"output_tokens": first}})),
        frame(
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": last}}),
        ),
        frame(json!({"type": "message_stop"})),
    ]
}

fn translator() -> StreamTranslator {
    StreamTranslator::new(
        ProtocolBridge::Passthrough,
        ProtocolKind::Messages,
        "client",
    )
    .unwrap()
}

fn assert_usage(stream: &StreamTranslator, input: u64, output: u64, text: &str) {
    let usage = &stream.metrics().usage;
    assert_eq!(usage.input_tokens, input);
    assert_eq!(usage.output_tokens, output);
    assert_eq!(usage.total_tokens, input + output);
    assert_eq!(usage.cache_read_tokens, 20);
    assert_eq!(usage.cache_write_tokens, 10);
    let semantic = stream.semantic_response();
    let usage = semantic.response.usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, input);
    assert_eq!(usage.output_tokens, output);
    assert_eq!(usage.cache_read_tokens, 20);
    assert_eq!(usage.cache_write_tokens, 10);
    let actual: String = semantic
        .response
        .candidates
        .iter()
        .flat_map(|candidate| &candidate.message.parts)
        .filter_map(|part| {
            if let LlmContentPart::Text { text, .. } = part {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(actual, text);
}

#[rstest::rstest]
#[case::later_count(7)]
#[case::explicit_zero(0)]
fn messages_partial_usage_keeps_omitted_fields_and_exact_forwarding(#[case] last: u64) {
    let input = frames(70, 5, last, "你好😀");
    let raw = input.concat();
    let mut stream = translator();
    let mut output = Vec::new();
    for (index, bytes) in input.iter().enumerate() {
        output.extend_from_slice(&stream.push_chunk(bytes).unwrap());
        if index == 4 {
            assert_eq!(stream.metrics().usage.output_tokens, 5);
        }
    }
    output.extend_from_slice(&stream.finish_stream().unwrap());
    assert_eq!(output, raw);
    assert_eq!(stream.upstream_snapshot(), raw);
    assert_usage(&stream, 70, last, "你好😀");
    assert!(stream.finish_stream().unwrap().is_empty());
}

#[rstest::rstest]
fn every_two_chunk_boundary_preserves_usage_and_utf8() {
    let raw = frames(70, 5, 0, "你好😀").concat();
    for split in 0..=raw.len() {
        let mut stream = translator();
        let mut output = stream.push_chunk(&raw[..split]).unwrap().to_vec();
        output.extend_from_slice(&stream.push_chunk(&raw[split..]).unwrap());
        output.extend_from_slice(&stream.finish_stream().unwrap());
        assert_eq!(output, raw, "split={split}");
        assert_usage(&stream, 70, 0, "你好😀");
    }
}

#[rstest::rstest]
fn separate_decoders_never_share_usage_state() {
    let mut first = translator();
    let mut second = translator();
    let first_frames = frames(70, 5, 7, "one");
    let second_frames = frames(30, 2, 0, "two");
    for (a, b) in first_frames.iter().zip(&second_frames) {
        first.push_chunk(a).unwrap();
        second.push_chunk(b).unwrap();
    }
    first.finish_stream().unwrap();
    second.finish_stream().unwrap();
    assert_usage(&first, 70, 7, "one");
    assert_usage(&second, 30, 0, "two");
}

#[rstest::rstest]
#[case::no_terminal("no-terminal", "without a protocol terminal")]
#[case::partial_frame("partial-frame", "inside an SSE frame")]
#[case::malformed_json("malformed-json", "semantic stream decode failed")]
fn incomplete_streams_keep_passthrough_bytes_but_cannot_finish(
    #[case] fault: &str,
    #[case] message: &str,
) {
    let mut input = frames(70, 5, 7, "partial");
    input.pop();
    let mut raw = input.concat();
    match fault {
        "no-terminal" => raw.extend_from_slice(b"data: [DONE]\n\n"),
        "partial-frame" => raw.extend_from_slice(b"data: {\"type\":\"message_stop\"}"),
        "malformed-json" => raw.extend_from_slice(b"data: {broken}\n\n"),
        _ => unreachable!(),
    }
    let mut stream = translator();
    assert_eq!(stream.push_chunk(&raw).unwrap(), raw);
    let error = stream.finish_stream().unwrap_err();
    assert!(error.to_string().contains(message), "{error}");
    if fault == "malformed-json" {
        stream
            .push_chunk(&frame(json!({"type": "message_stop"})))
            .unwrap();
        assert!(stream.finish_stream().is_err());
    }
}

proptest! {
    #[test]
    fn generated_chunks_keep_messages_usage_last_write_wins(
        input in 1u64..50_001,
        first in 0u64..50_001,
        last in 0u64..50_001,
        size in 1usize..65,
        text in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let text: String = text.into_iter().collect();
        let raw = frames(input, first, last, &text).concat();
        let mut stream = translator();
        let mut output = Vec::new();
        for chunk in raw.chunks(size) {
            output.extend_from_slice(&stream.push_chunk(chunk).unwrap());
        }
        output.extend_from_slice(&stream.finish_stream().unwrap());
        prop_assert_eq!(&output, &raw);
        prop_assert_eq!(stream.upstream_snapshot(), raw.as_slice());
        assert_usage(&stream, input, last, &text);
    }
}

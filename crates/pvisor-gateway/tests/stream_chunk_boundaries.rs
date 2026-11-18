//! Network chunk boundaries cannot change translated text or terminal events.
use pvisor_gateway::conversion::{ProtocolBridge, StreamTranslator};
use pvisor_gateway::protocol::ProtocolKind;
use serde_json::{Value, json};

fn translator() -> StreamTranslator {
    StreamTranslator::new(
        ProtocolBridge::MessagesToCompletions,
        ProtocolKind::Messages,
        "chunk-oracle",
    )
    .unwrap()
}

fn frame(value: Value, newline: &str) -> String {
    format!("data: {value}{newline}{newline}")
}

fn events(output: &[u8]) -> Vec<Value> {
    std::str::from_utf8(output)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn every_byte_split_and_single_byte_chunks_preserve_unicode_and_termination() {
    let first = "零\r\n\0\\\"";
    let second = "🦀尾";
    for newline in ["\n", "\r\n"] {
        let prefix = frame(
            json!({
                "id":"completion-chunks",
                "choices":[{"index":0,"delta":{"role":"assistant","content":first},"finish_reason":null}]
            }),
            newline,
        );
        let suffix = frame(
            json!({
                "id":"completion-chunks",
                "choices":[{"index":0,"delta":{"content":second},"finish_reason":"stop"}]
            }),
            newline,
        );
        let source =
            format!("{prefix}: heartbeat{newline}{newline}{suffix}data: [DONE]{newline}{newline}");
        for split in 0..=source.len() {
            let mut decoder = translator();
            let mut output = Vec::new();
            for part in [&source.as_bytes()[..split], &source.as_bytes()[split..]] {
                output.extend_from_slice(&decoder.push_chunk(part).unwrap());
            }
            output.extend_from_slice(&decoder.finish_stream().unwrap());
            assert_text_and_terminal(&events(&output), &format!("{first}{second}"));
        }
        let mut decoder = translator();
        let mut output = Vec::new();
        for byte in source.as_bytes().chunks(1) {
            output.extend_from_slice(&decoder.push_chunk(byte).unwrap());
        }
        output.extend_from_slice(&decoder.finish_stream().unwrap());
        assert_text_and_terminal(&events(&output), &format!("{first}{second}"));
    }
}

fn assert_text_and_terminal(events: &[Value], expected: &str) {
    let text: String = events
        .iter()
        .filter(|event| event["type"] == "content_block_delta")
        .map(|event| event["delta"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, expected);
    for kind in ["message_start", "message_delta", "message_stop"] {
        assert_eq!(
            events.iter().filter(|event| event["type"] == kind).count(),
            1,
            "{kind}: {events:?}"
        );
    }
    let terminal = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .unwrap();
    assert_eq!(terminal["delta"]["stop_reason"], "end_turn");
}

#[test]
fn truncated_unicode_frames_and_done_without_finish_reason_never_complete() {
    let prefix = frame(
        json!({
            "id":"completion-incomplete",
            "choices":[{"index":0,"delta":{"content":"零🦀尾"},"finish_reason":null}]
        }),
        "\n",
    );
    for end in 0..=prefix.len() {
        let mut decoder = translator();
        let output = decoder.push_chunk(&prefix.as_bytes()[..end]).unwrap();
        assert!(decoder.finish_stream().is_err(), "truncation={end}");
        assert!(
            events(&output)
                .iter()
                .all(|event| event["type"] != "message_stop"),
            "truncation={end}"
        );
    }
    let mut decoder = translator();
    let mut output = decoder.push_chunk(prefix.as_bytes()).unwrap().to_vec();
    output.extend_from_slice(&decoder.push_chunk(b"data: [DONE]\n\n").unwrap());
    assert!(decoder.finish_stream().is_err());
    assert!(
        events(&output)
            .iter()
            .all(|event| event["type"] != "message_stop")
    );
}

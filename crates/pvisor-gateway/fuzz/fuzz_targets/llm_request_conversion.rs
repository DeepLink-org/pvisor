#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use pvisor_gateway::conversion::{
    translate_request_for_bridge, translate_response_for_bridge, ProtocolBridge,
};
use pvisor_gateway::protocol::ProtocolKind;
use pvisor_gateway::understanding::understand_request;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let body = Bytes::copy_from_slice(body);
    // Exercise every production bridge, including direct Messages/Responses to
    // Gemini rendering rather than chaining through Chat Completions.
    let (bridge, protocol) = match selector % 5 {
        0 => (
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
        ),
        1 => (
            ProtocolBridge::ResponsesToCompletions,
            ProtocolKind::Responses,
        ),
        2 => (
            ProtocolBridge::CompletionsToGemini,
            ProtocolKind::ChatCompletions,
        ),
        3 => (ProtocolBridge::MessagesToGemini, ProtocolKind::Messages),
        _ => (ProtocolBridge::ResponsesToGemini, ProtocolKind::Responses),
    };
    if let Ok(parsed) = understand_request(protocol, &body) {
        if let Ok(wire) =
            translate_request_for_bridge(bridge, &parsed.semantic, "upstream-model", None)
        {
            let wire: serde_json::Value = serde_json::from_slice(&wire).unwrap();
            assert!(wire.is_object());
            if bridge.upstream_protocol(protocol) == ProtocolKind::ChatCompletions {
                assert_eq!(wire["model"], "upstream-model");
                assert!(wire["messages"].is_array());
            } else {
                assert!(wire["contents"].is_array());
            }
        }
    }
    if let Ok(translated) = translate_response_for_bridge(bridge, &body, protocol, "client-model") {
        let rendered: serde_json::Value = serde_json::from_slice(&translated.body).unwrap();
        assert!(rendered.is_object());
        assert_eq!(rendered["model"], "client-model");
        assert_eq!(
            translated.semantic.output_format,
            bridge.upstream_protocol(protocol).into()
        );
    }
});

use proptest::prelude::*;
use pvisor_guest::api::{
    EXIT_CLAIM_IOCTL, EXIT_REPORT_BYTES, EXIT_REPORT_IOCTL, EXIT_TOKEN_BYTES, GuestConfig,
    GuestExitReport, GuestFrameRead, GuestFrameWrite, GuestSignal, GuestValidation, MAX_FRAME,
    Reply, Request, SERIAL_MAX_REQUESTS, SerialCancel, SerialGuestFrame, SerialHostFrame,
    SerialInput, SerialInputAck, SerialReply, SerialRequest, SidecarProcess, StdinFrame,
};
use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read, Write},
};

fn launch() -> GuestConfig {
    GuestConfig {
        argv: vec!["/guest/program".into(), "$LITERAL; argument".into()],
        env: BTreeMap::from([("EXPLICIT".into(), "one".into())]),
        cwd: "/guest/work".into(),
        ..Default::default()
    }
}

#[test]
fn whole_launch_validation_is_read_only_and_checks_every_sidecar_before_effects() {
    assert_eq!(
        GuestConfig::default().validate().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let root = tempfile::tempdir().unwrap();
    let mut config = launch();
    config.sidecars.push(SidecarProcess {
        argv: vec!["/guest/sidecar".into()],
        env: BTreeMap::from([("EXPLICIT".into(), "sidecar".into())]),
        log: "/guest/sidecar.log".into(),
    });
    let before = serde_json::to_value(&config).unwrap();
    config.validate().unwrap();
    assert_eq!(serde_json::to_value(&config).unwrap(), before);
    for mutate in [
        |c: &mut GuestConfig| c.argv.clear(),
        |c: &mut GuestConfig| c.sidecars[0].argv[0].clear(),
        |c: &mut GuestConfig| {
            c.sidecars[0]
                .env
                .insert("INVALID=NAME".into(), "value".into());
        },
        |c: &mut GuestConfig| c.sidecars[0].log = "relative.log".into(),
        |c: &mut GuestConfig| {
            c.limits.insert("nofile".into(), (2, 1));
        },
    ] {
        let mut bad = config.clone();
        mutate(&mut bad);
        let before = serde_json::to_value(&bad).unwrap();
        assert_eq!(
            bad.validate().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(serde_json::to_value(&bad).unwrap(), before);
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn launch_json_rejects_unknown_fields_and_preserves_legacy_defaults() {
    let minimal = serde_json::json!({"argv":["/bin/true"],"env":{},"cwd":"/"});
    let config: GuestConfig = serde_json::from_value(minimal.clone()).unwrap();
    config.validate().unwrap();
    assert!(config.process.is_none() && config.network.is_none() && config.sidecars.is_empty());
    let mut unknown = minimal;
    unknown["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<GuestConfig>(unknown).is_err());
    for cwd in ["relative", "C:\\guest\\work", "\\guest\\work"] {
        let mut invalid = launch();
        invalid.cwd = cwd.into();
        assert_eq!(
            invalid.validate().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn control_ready_requires_the_guest_adapter_and_defaults_off() {
    let mut config = launch();
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("control_ready")
            .is_none()
    );
    config.control_ready = true;
    assert_eq!(
        config.validate().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    config.control = true;
    config.process = Some(pvisor_guest::api::ProcessConfig {
        new_session: true,
        ..Default::default()
    });
    config.validate().unwrap();
    let decoded: GuestConfig =
        serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
    assert!(decoded.control_ready && decoded.control);
}

fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: T) {
    let expected = serde_json::to_value(&value).unwrap();
    let mut bytes = Vec::new();
    bytes.write_frame(&value).unwrap();
    let decoded: T = bytes.as_slice().read_frame().unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
}

#[test]
fn control_and_serial_records_keep_their_wire_shapes_and_credit_identities() {
    let mut bytes = Vec::new();
    bytes.write_frame(&Request::Ping {}).unwrap();
    assert_eq!(&bytes[4..], br#"{"operation":"Ping"}"#);
    assert_eq!(
        u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize,
        bytes.len() - 4
    );
    assert_eq!(SERIAL_MAX_REQUESTS, 32);
    for frame in [
        SerialHostFrame::Request(SerialRequest {
            id: 1,
            request: Request::Ping {},
        }),
        SerialHostFrame::Input(SerialInput {
            input_id: 1,
            input: StdinFrame::Data {
                bytes: vec![0, 255],
            },
        }),
        SerialHostFrame::Cancel(SerialCancel { cancel_id: 1 }),
    ] {
        roundtrip(frame);
    }
    for frame in [
        SerialGuestFrame::Reply(SerialReply {
            id: 1,
            reply: Reply::Ready,
        }),
        SerialGuestFrame::InputAck(SerialInputAck { input_ack: 1 }),
    ] {
        roundtrip(frame);
    }
}

#[test]
fn frame_bound_is_inclusive_and_rejection_does_not_touch_the_writer() {
    let value = "x".repeat(MAX_FRAME - 2);
    let mut bytes = Vec::new();
    bytes.write_frame(&value).unwrap();
    assert_eq!(bytes.len(), MAX_FRAME + 4);
    assert_eq!(bytes.as_slice().read_frame::<String>().unwrap(), value);
    let mut untouched = vec![7, 8, 9];
    assert_eq!(
        untouched
            .write_frame(&"x".repeat(MAX_FRAME - 1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(untouched, [7, 8, 9]);
    struct Unserializable;
    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("serialization fixture"))
        }
    }
    assert!(untouched.write_frame(&Unserializable).is_err());
    assert_eq!(untouched, [7, 8, 9]);
}

#[test]
fn frame_errors_preserve_bound_and_stream_position_without_authorizing_a_request() {
    let mut oversized =
        Cursor::new([((MAX_FRAME as u32) + 1).to_be_bytes().as_slice(), b"unread"].concat());
    assert_eq!(
        oversized.read_frame::<Request>().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(oversized.position(), 4);
    for header_bytes in 0..4 {
        let mut truncated = Cursor::new(vec![0; header_bytes]);
        assert_eq!(
            truncated.read_frame::<Request>().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    let mut truncated = Cursor::new([5_u32.to_be_bytes().as_slice(), b"{}"].concat());
    assert_eq!(
        truncated.read_frame::<Request>().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    for (json, kind) in [
        (b"!".as_slice(), io::ErrorKind::InvalidData),
        (b"{".as_slice(), io::ErrorKind::UnexpectedEof),
        (
            br#"{"operation":"Ping","unknown":true}"#.as_slice(),
            io::ErrorKind::InvalidData,
        ),
        (
            br#"{"operation":"unknown"}"#.as_slice(),
            io::ErrorKind::InvalidData,
        ),
    ] {
        let mut reader = Cursor::new([(json.len() as u32).to_be_bytes().as_slice(), json].concat());
        assert_eq!(reader.read_frame::<Request>().unwrap_err().kind(), kind);
        assert_eq!(reader.position() as usize, 4 + json.len());
    }
    roundtrip(Request::Stop { signal: 0 });
}

#[test]
fn fragmented_and_interrupted_reads_keep_consecutive_frames_separate() {
    struct Fragmented {
        bytes: Cursor<Vec<u8>>,
        interrupted: bool,
    }
    impl Read for Fragmented {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let size = buf.len().min(1);
            self.bytes.read(&mut buf[..size])
        }
    }
    let mut bytes = Vec::new();
    bytes.write_frame(&1_u32).unwrap();
    bytes.write_frame(&2_u32).unwrap();
    let size = bytes.len();
    let mut stream = Fragmented {
        bytes: Cursor::new(bytes),
        interrupted: false,
    };
    assert_eq!(stream.read_frame::<u32>().unwrap(), 1);
    assert_eq!(stream.read_frame::<u32>().unwrap(), 2);
    assert_eq!(stream.bytes.position() as usize, size);
}

#[test]
fn partial_frame_write_propagates_error_without_restarting_or_flushing() {
    struct Budget {
        bytes: Vec<u8>,
        calls: usize,
        remaining: usize,
    }
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.remaining == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let size = self.remaining.min(bytes.len());
            self.bytes.extend_from_slice(&bytes[..size]);
            self.remaining -= size;
            Ok(size)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("framing does not own flush")
        }
    }
    let mut expected = Vec::new();
    expected.write_frame(&"body").unwrap();
    let mut stream = Budget {
        bytes: Vec::new(),
        calls: 0,
        remaining: 7,
    };
    assert_eq!(
        stream.write_frame(&"body").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(stream.bytes, expected[..7]);
    assert_eq!(stream.calls, 3);
}

#[test]
fn linux_signal_parser_is_host_independent_and_rejects_unsupported_spellings() {
    for number in 1..=64 {
        assert_eq!(number.to_string().parse_signal().unwrap(), number);
    }
    for (name, expected) in [
        ("", 15),
        ("SIG", 15),
        ("TERM", 15),
        ("KILL", 9),
        ("INT", 2),
        ("QUIT", 3),
        ("HUP", 1),
        ("USR1", 10),
        ("USR2", 12),
        ("WINCH", 28),
    ] {
        assert_eq!(name.parse_signal().unwrap(), expected);
        assert_eq!(
            format!("SIG{name}").parse_signal().is_ok(),
            !name.starts_with("SIG")
        );
    }
    for invalid in [
        "0",
        "65",
        "-1",
        "term",
        "sigterm",
        " 15",
        "15 ",
        "SIGSIGTERM",
    ] {
        assert_eq!(
            invalid.parse_signal().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn exit_reports_keep_fixed_wire_bytes_and_validate_before_copying_a_token() {
    assert_eq!(EXIT_CLAIM_IOCTL, 0x8010_7603);
    assert_eq!(EXIT_REPORT_IOCTL, 0x4014_7604);
    assert_eq!((EXIT_TOKEN_BYTES, EXIT_REPORT_BYTES), (16, 20));
    let token = [0xa5; EXIT_TOKEN_BYTES];
    for code in 0..=255 {
        let report = token.report(code).unwrap();
        assert_eq!(&report[..16], &token);
        assert_eq!(&report[16..], &(code as u32).to_le_bytes());
    }
    for code in [i32::MIN, -1, 256, i32::MAX] {
        assert_eq!(
            token.report(code).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(token, [0xa5; 16]);
}

proptest! {
    #[test]
    fn generated_launch_values_survive_validation_and_nul_is_rejected(
        args in prop::collection::vec("[^\0]{0,64}", 0..8),
        key in "[A-Z][A-Z0-9_]{0,15}",
        value in "[^\0]{0,64}",
        directory in "[a-z]{1,16}",
    ) {
        let mut config = launch();
        config.argv.extend(args);
        config.env.insert(key.clone(), value);
        config.cwd = format!("/guest/{directory}").into();
        let before = serde_json::to_value(&config).unwrap();
        config.validate().unwrap();
        prop_assert_eq!(serde_json::to_value(&config).unwrap(), before);
        let decoded: GuestConfig = serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        decoded.validate().unwrap();
        prop_assert_eq!(decoded.argv, config.argv.clone());
        prop_assert_eq!(decoded.env, config.env.clone());
        config.argv.push(format!("{directory}\0"));
        prop_assert_eq!(config.validate().unwrap_err().kind(), io::ErrorKind::InvalidInput);
        config.argv.pop();
        config.env.insert(format!("{key}="), "value".into());
        prop_assert_eq!(config.validate().unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn every_sidecar_is_validated_before_launch(
        count in 1usize..5,
        selected in any::<usize>(),
        value in "[^\0]{0,64}",
    ) {
        let mut config = launch();
        config.sidecars = (0..count).map(|index| SidecarProcess {
            argv: vec!["/guest/helper".into(), value.clone()],
            env: BTreeMap::new(),
            log: format!("/guest/helper-{index}.log").into(),
        }).collect();
        config.validate().unwrap();
        let index = selected % count;
        config.sidecars[index].argv[1].push('\0');
        let before = serde_json::to_value(&config).unwrap();
        prop_assert_eq!(config.validate().unwrap_err().kind(), io::ErrorKind::InvalidInput);
        prop_assert_eq!(serde_json::to_value(&config).unwrap(), before);
    }

    #[test]
    fn resource_limit_order_determines_admission(soft in any::<u64>(), hard in any::<u64>()) {
        let mut config = launch();
        config.limits.insert("nofile".into(), (soft, hard));
        let before = serde_json::to_value(&config).unwrap();
        let result = config.validate();
        prop_assert_eq!(result.is_ok(), soft <= hard);
        if let Err(error) = result {
            prop_assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        prop_assert_eq!(serde_json::to_value(&config).unwrap(), before);
    }

    #[test]
    fn consecutive_frames_preserve_payloads_and_credit_ids(
        id in any::<u64>(),
        payload in prop::collection::vec(any::<u8>(), 0..1024),
        cancel_id in any::<u64>(),
    ) {
        let frame = SerialHostFrame::Input(SerialInput {
            input_id: id,
            input: StdinFrame::Data { bytes: payload },
        });
        let json = serde_json::to_vec(&frame).unwrap();
        let mut bytes = Vec::new();
        bytes.write_frame(&frame).unwrap();
        prop_assert_eq!(&bytes[..4], &(json.len() as u32).to_be_bytes());
        prop_assert_eq!(&bytes[4..], &json);
        let first_end = bytes.len();
        bytes.write_frame(&SerialHostFrame::Cancel(SerialCancel { cancel_id })).unwrap();
        let end = bytes.len();
        let mut reader = Cursor::new(bytes);
        let decoded: SerialHostFrame = reader.read_frame().unwrap();
        prop_assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(frame).unwrap());
        prop_assert_eq!(reader.position() as usize, first_end);
        let decoded: SerialHostFrame = reader.read_frame().unwrap();
        prop_assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::json!({"cancel_id": cancel_id}));
        prop_assert_eq!(reader.position() as usize, end);
    }

    #[test]
    fn truncated_frames_return_eof(value in ".{0,64}", selected in any::<usize>()) {
        let mut bytes = Vec::new();
        bytes.write_frame(&value).unwrap();
        let cut = selected % bytes.len();
        let mut reader = Cursor::new(&bytes[..cut]);
        prop_assert_eq!(reader.read_frame::<String>().unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        prop_assert_eq!(reader.position() as usize, cut);
    }

    #[test]
    fn oversized_headers_never_consume_payload(
        size in (MAX_FRAME as u32 + 1)..=u32::MAX,
        payload in prop::collection::vec(any::<u8>(), 0..128),
    ) {
        let mut reader = Cursor::new([size.to_be_bytes().as_slice(), &payload].concat());
        prop_assert_eq!(reader.read_frame::<Request>().unwrap_err().kind(), io::ErrorKind::InvalidData);
        prop_assert_eq!(reader.position(), 4);
    }

    #[test]
    fn numeric_signals_are_exactly_linux_range(number in prop_oneof![-128i32..=128, any::<i32>()]) {
        let result = number.to_string().parse_signal();
        if (1..=64).contains(&number) {
            prop_assert_eq!(result.unwrap(), number);
        } else {
            prop_assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn exit_report_preserves_any_token_and_rejects_invalid_codes(
        token in any::<[u8; EXIT_TOKEN_BYTES]>(),
        code in prop_oneof![0i32..=255, any::<i32>()],
    ) {
        let before = token;
        match token.report(code) {
            Ok(report) => {
                prop_assert!((0..=255).contains(&code));
                prop_assert_eq!(&report[..EXIT_TOKEN_BYTES], &token);
                prop_assert_eq!(&report[EXIT_TOKEN_BYTES..], &(code as u32).to_le_bytes());
            }
            Err(error) => {
                prop_assert!(!(0..=255).contains(&code));
                prop_assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            }
        }
        prop_assert_eq!(token, before);
    }
}

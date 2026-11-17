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

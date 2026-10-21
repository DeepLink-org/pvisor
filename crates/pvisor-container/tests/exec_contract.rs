use pvisor_container::api::{
    CancelExec, ExecOutcome, ExecRelayError, ExecRequest, ExecRuntime, ExecSession, ExecStartError,
};
use pvisor_guest::control::{Reply, Request, StdinFrame, read_frame, write_frame};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    time::Duration,
};

fn outcome(code: i32) -> ExecOutcome {
    ExecOutcome {
        code,
        signal: None,
        output_complete: true,
    }
}

fn pair() -> (UnixStream, UnixStream) {
    let (client, guest) = UnixStream::pair().unwrap();
    guest
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (client, guest)
}
fn accept(guest: &mut UnixStream) -> ExecRequest {
    let Request::Exec(request) = read_frame(guest).unwrap() else {
        panic!("expected exec")
    };
    write_frame(guest, &Reply::Started { pid: 42 }).unwrap();
    request
}

fn session(
    request: ExecRequest,
    serve: impl FnOnce(UnixStream, ExecRequest) + Send + 'static,
) -> (ExecSession, std::thread::JoinHandle<()>) {
    let (client, mut guest) = pair();
    let worker = std::thread::spawn(move || {
        let request = accept(&mut guest);
        serve(guest, request);
    });
    (ExecSession::start(client, request).unwrap(), worker)
}

fn relay(session: ExecSession, input: Option<File>) -> Result<ExecOutcome, ExecRelayError> {
    session.relay(input, &mut Vec::new(), &mut Vec::new())
}

fn input_file(bytes: &[u8]) -> File {
    let mut input = tempfile::tempfile().unwrap();
    input.write_all(bytes).unwrap();
    input.rewind().unwrap();
    input
}

fn expect_eof(guest: &mut UnixStream) {
    assert!(matches!(read_frame(guest).unwrap(), StdinFrame::Eof {}));
}

fn output(guest: &mut UnixStream, stream: u8, bytes: &[u8]) {
    write_frame(
        guest,
        &Reply::Output {
            stream,
            bytes: bytes.to_vec(),
        },
    )
    .unwrap();
}

#[test]
fn eof_keeps_output_open_and_preserves_pid_streams_and_exit() {
    let (session, worker) = session(
        ExecRequest {
            argv: vec!["cat".into()],
            replace_env: true,
            limits: Some(std::collections::BTreeMap::from([(
                "RLIMIT_NOFILE".into(),
                (32, 64),
            )])),
            ..Default::default()
        },
        |mut guest, request| {
            assert!(request.replace_env);
            assert_eq!(request.limits.unwrap()["RLIMIT_NOFILE"], (32, 64));
            let mut input = Vec::new();
            while let StdinFrame::Data { bytes } = read_frame(&mut guest).unwrap() {
                input.extend(bytes);
            }
            assert_eq!(input, b"tail\0\xff");
            output(&mut guest, 1, &input);
            output(&mut guest, 2, b"diagnostic");
            write_frame(&mut guest, &Reply::ExecExit(outcome(23))).unwrap();
        },
    );
    assert_eq!(session.pid(), 42);
    let input = input_file(b"tail\0\xff");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        session.relay(Some(input), &mut out, &mut err).unwrap(),
        outcome(23)
    );
    assert_eq!(out, b"tail\0\xff");
    assert_eq!(err, b"diagnostic");
    worker.join().unwrap();
}

#[test]
fn idle_input_is_released_when_guest_exits() {
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        write_frame(&mut guest, &Reply::ExecExit(outcome(0))).unwrap();
    });
    let (input, _keep_open) = UnixStream::pair().unwrap();
    let input = File::from(OwnedFd::from(input));
    let (done, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(relay(session, Some(input))).unwrap();
    });
    assert_eq!(
        received
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap(),
        outcome(0)
    );
    worker.join().unwrap();
}

#[test]
fn cancellation_works_after_explicit_stdin_eof() {
    let (eof, received) = std::sync::mpsc::channel();
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        expect_eof(&mut guest);
        eof.send(()).unwrap();
        assert_eq!(guest.read(&mut [0]).unwrap(), 0);
    });
    let cancellation = session.cancellation().unwrap();
    let relay = std::thread::spawn(move || relay(session, None));
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    cancellation.cancel().unwrap();
    cancellation.cancel().unwrap();
    assert!(relay.join().unwrap().is_err());
    worker.join().unwrap();
}

#[test]
fn rejected_spawn_preserves_execution_error_code() {
    let (client, mut guest) = pair();
    let worker = std::thread::spawn(move || {
        let _: Request = read_frame(&mut guest).unwrap();
        write_frame(
            &mut guest,
            &Reply::Error {
                message: "missing executable".into(),
            },
        )
        .unwrap();
        write_frame(&mut guest, &Reply::Exit { code: 127 }).unwrap();
    });
    assert!(matches!(
        ExecSession::start(client, ExecRequest::default()),
        Err(ExecStartError::Rejected { code: 127, .. })
    ));
    worker.join().unwrap();
}

#[test]
fn dropping_a_session_cancels_even_with_an_outstanding_handle() {
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        assert_eq!(guest.read(&mut [0]).unwrap(), 0);
    });
    let _handle = session.cancellation().unwrap();
    drop(session);
    worker.join().unwrap();
}

#[test]
fn invalid_output_channel_is_not_a_successful_exit() {
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        expect_eof(&mut guest);
        output(&mut guest, 9, &[]);
    });
    assert!(matches!(
        relay(session, None),
        Err(ExecRelayError::Transport(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    worker.join().unwrap();
}

#[test]
fn fast_exit_is_preserved_when_guest_has_already_closed_stdin() {
    let (client, mut guest) = pair();
    let (done, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        accept(&mut guest);
        write_frame(&mut guest, &Reply::ExecExit(outcome(23))).unwrap();
        drop(guest);
        done.send(()).unwrap();
    });
    let session = ExecSession::start(client, ExecRequest::default()).unwrap();
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(relay(session, None).unwrap(), outcome(23));
    worker.join().unwrap();
}

#[test]
fn slow_stdin_consumer_is_not_cancelled_after_two_seconds() {
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        // Larger than the socket send buffer; old two-second writes truncated it.
        std::thread::sleep(Duration::from_millis(2400));
        let mut count = 0;
        while let StdinFrame::Data { bytes } = read_frame(&mut guest).unwrap() {
            assert!(bytes.iter().all(|byte| *byte == b'x'));
            count += bytes.len();
        }
        assert_eq!(count, 2 * 1024 * 1024);
        write_frame(&mut guest, &Reply::ExecExit(outcome(0))).unwrap();
    });
    let input = input_file(&vec![b'x'; 2 * 1024 * 1024]);
    assert_eq!(relay(session, Some(input)).unwrap(), outcome(0));
    worker.join().unwrap();
}

#[test]
fn signal_and_incomplete_output_facts_are_not_flattened_into_an_exit_code() {
    for expected in [
        outcome(143),
        ExecOutcome {
            signal: Some(15),
            ..outcome(143)
        },
        ExecOutcome {
            output_complete: false,
            ..outcome(0)
        },
    ] {
        let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
            expect_eof(&mut guest);
            write_frame(&mut guest, &Reply::ExecExit(expected)).unwrap();
        });
        assert_eq!(relay(session, None).unwrap(), expected);
        worker.join().unwrap();
    }
}

#[test]
fn output_failure_is_distinct_from_transport_failure_and_cancels_the_session() {
    struct FailedOutput;
    impl Write for FailedOutput {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        expect_eof(&mut guest);
        output(&mut guest, 1, b"output");
        assert_eq!(guest.read(&mut [0]).unwrap(), 0);
    });
    assert!(matches!(
        session.relay(None, &mut FailedOutput, &mut Vec::new()),
        Err(ExecRelayError::Output(_))
    ));
    worker.join().unwrap();
}

#[test]
fn startup_honors_the_callers_socket_timeout() {
    let (client, mut guest) = pair();
    client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        let _: Request = read_frame(&mut guest).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let _ = write_frame(&mut guest, &Reply::Started { pid: 42 });
    });
    assert!(matches!(ExecSession::start(client, ExecRequest::default()),
        Err(ExecStartError::Transport(error)) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)));
    worker.join().unwrap();
}

#[test]
fn local_input_read_failure_closes_stdin_but_keeps_output_connected() {
    let (session, worker) = session(ExecRequest::default(), move |mut guest, _| {
        expect_eof(&mut guest);
        write_frame(&mut guest, &Reply::ExecExit(outcome(0))).unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let input = File::open(directory.path()).unwrap();
    assert_eq!(relay(session, Some(input)).unwrap(), outcome(0));
    worker.join().unwrap();
}

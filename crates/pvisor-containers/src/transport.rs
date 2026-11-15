//! Bounded request multiplexing over one duplex guest console. A slow client
//! fails its own delivery instead of blocking unrelated execs or growing RAM.
use anyhow::{Context, ensure};
#[cfg(all(test, unix))]
use pvisor_guest::control::SerialReply;
use pvisor_guest::control::{
    Reply, Request, SERIAL_MAX_REQUESTS, SerialCancel, SerialGuestFrame, SerialInput,
    SerialRequest, StdinFrame, read_frame, write_frame,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

const OUTPUT_FRAMES: usize = 64;
struct Pending {
    sender: mpsc::SyncSender<Reply>,
    failure: Arc<Mutex<Option<String>>>,
    kind: Kind,
    credit: Arc<Credit>,
}
#[derive(Clone, Copy)]
enum Kind {
    Exec,
    Input(bool),
    Other,
}
impl Kind {
    fn of(request: &Request) -> Self {
        match request {
            Request::Exec { .. } | Request::Start { .. } => Self::Exec,
            Request::Input {} => Self::Input(false),
            _ => Self::Other,
        }
    }
    fn terminal(&mut self, reply: &Reply) -> bool {
        if matches!(reply, Reply::Exit { .. } | Reply::Failure { .. }) {
            return true;
        }
        if !matches!(reply, Reply::Ready) {
            return false;
        }
        match self {
            Self::Exec => false,
            Self::Other => true,
            Self::Input(ready) => std::mem::replace(ready, true),
        }
    }
}
struct Credit {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Credit {
    fn close(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}
#[derive(Clone)]
pub(crate) struct InputHandle {
    broker: Arc<Broker>,
    id: u64,
    credit: Arc<Credit>,
    eof: Arc<AtomicBool>,
}
pub(crate) struct Ticket {
    input: InputHandle,
    receiver: mpsc::Receiver<Reply>,
    failure: Arc<Mutex<Option<String>>>,
    kind: Kind,
    complete: bool,
}
#[derive(Default)]
struct State {
    pending: BTreeMap<u64, Pending>,
    closed: Option<String>,
    ready: bool,
}
struct Writer {
    stream: Box<dyn Write + Send>,
    next: u64,
}
pub(crate) struct Broker {
    writer: Mutex<Writer>,
    state: Arc<Mutex<State>>,
    started: Arc<Condvar>,
}

impl Broker {
    pub(crate) fn new(
        mut reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Arc<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let input = state.clone();
        let started = Arc::new(Condvar::new());
        let notification = started.clone();
        std::thread::spawn(move || {
            let error = loop {
                let frame = match read_frame::<SerialGuestFrame>(&mut reader) {
                    Ok(SerialGuestFrame::Reply(frame)) => frame,
                    Ok(SerialGuestFrame::InputAck(ack)) => {
                        let state = input.lock().unwrap();
                        if let Some(pending) = state.pending.get(&ack.input_ack) {
                            pending.credit.state.lock().unwrap().0 = true;
                            pending.credit.changed.notify_all();
                        }
                        continue;
                    }
                    Err(error) => break error.to_string(),
                };
                if matches!(&frame.reply, Reply::Output { bytes, .. } if bytes.len() > 16 * 1024) {
                    break "guest output chunk exceeds 16 KiB".into();
                }
                let mut state = input.lock().unwrap();
                if frame.id == 0 && matches!(frame.reply, Reply::Ready) {
                    state.ready = true;
                    notification.notify_all();
                    continue;
                }
                if let Some(pending) = state.pending.get_mut(&frame.id) {
                    let terminal = pending.kind.terminal(&frame.reply);
                    if pending.sender.try_send(frame.reply).is_err() {
                        *pending.failure.lock().unwrap() = Some(
                            "exec output consumer disconnected or exceeded its bounded backlog"
                                .into(),
                        );
                        pending.credit.close();
                        state.pending.remove(&frame.id);
                    } else if terminal {
                        pending.credit.close();
                        state.pending.remove(&frame.id);
                    }
                }
                // Replies to abandoned requests are drained, not attributed to
                // a later execution. IDs are never reused within a connection.
            };
            Self::close(&input, error);
            notification.notify_all();
        });
        Arc::new(Self {
            writer: Mutex::new(Writer {
                stream: Box::new(writer),
                next: 0,
            }),
            state,
            started,
        })
    }

    pub(crate) fn wait_ready(&self) -> anyhow::Result<()> {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .started
            .wait_timeout_while(state, std::time::Duration::from_secs(120), |state| {
                !state.ready && state.closed.is_none()
            })
            .unwrap();
        ensure!(
            state.ready && !timeout.timed_out(),
            "guest supervisor did not confirm launch: {:?}",
            state.closed
        );
        Ok(())
    }
    #[cfg(windows)]
    pub(crate) fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed.is_some()
    }
    fn close(state: &Mutex<State>, error: String) {
        let mut state = state.lock().unwrap();
        state.closed = Some(error.clone());
        for (_, pending) in std::mem::take(&mut state.pending) {
            *pending.failure.lock().unwrap() = Some(error.clone());
            pending.credit.close();
        }
    }

    pub(crate) fn begin(self: &Arc<Self>, request: Request) -> anyhow::Result<Ticket> {
        let kind = Kind::of(&request);
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_FRAMES);
        let failure = Arc::new(Mutex::new(None));
        let credit = Arc::new(Credit {
            state: Mutex::new((true, false)),
            changed: Condvar::new(),
        });
        let id;
        {
            // Allocate IDs under the frame writer lock: concurrent callers
            // cannot send IDs out of order or interleave frame bytes.
            let mut writer = self.writer.lock().unwrap();
            let mut state = self.state.lock().unwrap();
            ensure!(
                state.closed.is_none(),
                "guest control disconnected: {:?}",
                state.closed
            );
            ensure!(
                state.pending.len() < SERIAL_MAX_REQUESTS,
                "too many concurrent guest requests"
            );
            writer.next = writer
                .next
                .checked_add(1)
                .context("guest request identities exhausted")?;
            id = writer.next;
            state.pending.insert(
                id,
                Pending {
                    sender,
                    failure: failure.clone(),
                    kind,
                    credit: credit.clone(),
                },
            );
            drop(state);
            if let Err(error) = write_frame(&mut writer.stream, &SerialRequest { id, request }) {
                Self::close(&self.state, error.to_string());
                self.started.notify_all();
                return Err(error.into());
            }
        }
        Ok(Ticket {
            input: InputHandle {
                broker: self.clone(),
                id,
                credit,
                eof: Arc::new(AtomicBool::new(false)),
            },
            receiver,
            failure,
            kind,
            complete: false,
        })
    }
    pub(crate) fn exchange(
        self: &Arc<Self>,
        request: Request,
        output: &mut impl Write,
    ) -> anyhow::Result<()> {
        let exec = matches!(request, Request::Exec { .. } | Request::Start { .. });
        let mut ticket = self.begin(request)?;
        if exec {
            ticket.input.send(StdinFrame::Eof {})?;
        }
        let mut output_error = None;
        loop {
            let reply = ticket.recv()?;
            if output_error.is_none()
                && let Err(error) = write_frame(output, &reply)
            {
                output_error = Some(error);
            }
            if ticket.complete {
                return output_error.map_or(Ok(()), |error| Err(error.into()));
            }
        }
    }
}

impl InputHandle {
    pub(crate) fn send(&self, input: StdinFrame) -> anyhow::Result<()> {
        ensure!(!self.eof.load(Ordering::Acquire), "stdin is already closed");
        if let StdinFrame::Data { bytes } = &input {
            ensure!(bytes.len() <= 16384, "stdin chunk exceeds 16 KiB");
        }
        let state = self.credit.state.lock().unwrap();
        let mut state = self
            .credit
            .changed
            .wait_while(state, |(available, closed)| !*available && !*closed)
            .unwrap();
        ensure!(!state.1, "guest stdin attachment ended");
        state.0 = false;
        drop(state);
        if matches!(input, StdinFrame::Eof {}) {
            self.eof.store(true, Ordering::Release);
        }
        let mut writer = self.broker.writer.lock().unwrap();
        if let Err(error) = write_frame(
            &mut writer.stream,
            &SerialInput {
                input_id: self.id,
                input,
            },
        ) {
            Broker::close(&self.broker.state, error.to_string());
            return Err(error.into());
        }
        Ok(())
    }
    pub(crate) fn cancel(&self) {
        self.credit.close();
        if !self.eof.load(Ordering::Acquire) {
            let mut writer = self.broker.writer.lock().unwrap();
            let _ = write_frame(&mut writer.stream, &SerialCancel { cancel_id: self.id });
        }
    }
}
impl Ticket {
    pub(crate) fn input(&self) -> InputHandle {
        self.input.clone()
    }
    pub(crate) fn recv(&mut self) -> anyhow::Result<Reply> {
        let reply = self.receiver.recv().map_err(|_| {
            anyhow::anyhow!(
                "guest reply incomplete: {}",
                self.failure
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("missing terminal frame")
            )
        })?;
        self.complete = self.kind.terminal(&reply);
        Ok(reply)
    }
    pub(crate) fn complete(&self) -> bool {
        self.complete
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.input
            .broker
            .state
            .lock()
            .unwrap()
            .pending
            .remove(&self.input.id);
        if !self.complete {
            self.input.cancel();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixStream, time::Duration};

    #[test]
    fn bootstrap_receipt_survives_a_fast_exit_and_stdin_is_credit_bounded() {
        use pvisor_guest::control::{ExecLaunch, SerialInputAck};
        let (host, mut guest) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let broker = Broker::new(host.try_clone().unwrap(), host);
        write_frame(
            &mut guest,
            &SerialReply {
                id: 0,
                reply: Reply::Ready,
            },
        )
        .unwrap();
        broker.wait_ready().unwrap();
        let mut ticket = broker
            .begin(Request::Start {
                launch: ExecLaunch {
                    argv: vec!["/bin/cat".into()],
                    env: Default::default(),
                    replace_env: false,
                    limits: Default::default(),
                    cwd: None,
                    user: None,
                    tty: Some([24, 80]),
                },
            })
            .unwrap();
        let request: SerialRequest = read_frame(&mut guest).unwrap();
        write_frame(
            &mut guest,
            &SerialReply {
                id: request.id,
                reply: Reply::Started {
                    pid: 42,
                    generation: 1,
                },
            },
        )
        .unwrap();
        assert!(matches!(
            ticket.recv().unwrap(),
            Reply::Started { pid: 42, .. }
        ));
        assert!(!ticket.complete());
        let input = ticket.input();
        input
            .send(StdinFrame::Data {
                bytes: b"one".to_vec(),
            })
            .unwrap();
        let first: SerialInput = read_frame(&mut guest).unwrap();
        assert_eq!(first.input_id, request.id);
        let (entered, waiting) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            let result = input.send(StdinFrame::Data {
                bytes: b"two".to_vec(),
            });
            finished.send(result).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            done.try_recv().is_err(),
            "input advanced without a returned credit"
        );
        write_frame(
            &mut guest,
            &SerialInputAck {
                input_ack: request.id,
            },
        )
        .unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        let second: SerialInput = read_frame(&mut guest).unwrap();
        assert!(matches!(second.input, StdinFrame::Data { bytes } if bytes == b"two"));
        write_frame(
            &mut guest,
            &SerialReply {
                id: request.id,
                reply: Reply::Exit { code: 0 },
            },
        )
        .unwrap();
        assert!(matches!(ticket.recv().unwrap(), Reply::Exit { code: 0 }));
        assert!(ticket.complete());
        assert!(ticket.input().send(StdinFrame::Eof {}).is_err());
        guest.shutdown(std::net::Shutdown::Both).unwrap();
        broker.wait_ready().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn interleaved_replies_reach_only_their_request_and_eof_fails_pending_calls() {
        let (host, mut guest) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let broker = Broker::new(host.try_clone().unwrap(), host);
        let first = {
            let broker = broker.clone();
            std::thread::spawn(move || {
                let mut out = Vec::new();
                broker.exchange(Request::Ping {}, &mut out)?;
                Ok::<_, anyhow::Error>(out)
            })
        };
        let a: SerialRequest = read_frame(&mut guest).unwrap();
        let second = {
            let broker = broker.clone();
            std::thread::spawn(move || {
                let mut out = Vec::new();
                broker.exchange(Request::Ping {}, &mut out)?;
                Ok::<_, anyhow::Error>(out)
            })
        };
        let b: SerialRequest = read_frame(&mut guest).unwrap();
        assert!(a.id > 0 && b.id > a.id);
        write_frame(
            &mut guest,
            &SerialReply {
                id: b.id,
                reply: Reply::Ready,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<Reply>(&mut second.join().unwrap().unwrap().as_slice()).unwrap(),
            Reply::Ready
        ));
        guest.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(first.join().unwrap().is_err());
    }

    #[test]
    fn stalled_output_is_bounded_and_does_not_block_another_request() {
        struct Stalled {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
        }
        impl Write for Stalled {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (host, mut guest) = UnixStream::pair().unwrap();
        guest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let broker = Broker::new(host.try_clone().unwrap(), host);
        let (entered, waiting) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let stalled = {
            let broker = broker.clone();
            std::thread::spawn(move || {
                broker.exchange(
                    Request::Exec {
                        argv: vec!["/bin/true".into()],
                        env: Default::default(),
                        unset_env: Vec::new(),
                        cwd: None,
                        user: None,
                        tty: None,
                    },
                    &mut Stalled {
                        entered: Some(entered),
                        release: proceed,
                    },
                )
            })
        };
        let a: SerialRequest = read_frame(&mut guest).unwrap();
        let input: SerialInput = read_frame(&mut guest).unwrap();
        assert_eq!(input.input_id, a.id);
        assert!(matches!(input.input, StdinFrame::Eof {}));
        write_frame(
            &mut guest,
            &SerialReply {
                id: a.id,
                reply: Reply::Ready,
            },
        )
        .unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..=OUTPUT_FRAMES {
            write_frame(
                &mut guest,
                &SerialReply {
                    id: a.id,
                    reply: Reply::Output {
                        stream: 1,
                        bytes: vec![1],
                    },
                },
            )
            .unwrap();
        }
        let fast = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.exchange(Request::Ping {}, &mut Vec::new()))
        };
        let b: SerialRequest = read_frame(&mut guest).unwrap();
        write_frame(
            &mut guest,
            &SerialReply {
                id: b.id,
                reply: Reply::Ready,
            },
        )
        .unwrap();
        fast.join().unwrap().unwrap();
        release.send(()).unwrap();
        assert!(
            stalled
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("bounded backlog")
        );
        guest.shutdown(std::net::Shutdown::Both).unwrap();
    }
}

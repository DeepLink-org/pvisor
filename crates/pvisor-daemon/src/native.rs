//! Host protocol adapters over the host-only pvisor-guest control engine.
//! No workload or file operation executes on the host. A detached supervisor
//! owns jobs and bounded replay buffers for the life of exactly one VM.
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use pvisor_guest::control::{
    ExecLaunch, FileRequest, MAX_FRAME, Reply, Request as GuestRequest, StdinFrame,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex as AsyncMutex, watch},
};

mod e2b;
mod opensandbox;

#[derive(Clone)]
pub(crate) struct Service {
    pub path: PathBuf,
    pub token: String,
    pub sandbox_id: String,
    pub generation: String,
    pub generation_number: u64,
    pub env: BTreeMap<String, String>,
    pub e2b: bool,
    jobs: Arc<Mutex<VecDeque<Arc<Job>>>>,
    slots: Arc<tokio::sync::Semaphore>,
    http_slots: Arc<tokio::sync::Semaphore>,
}
impl Service {
    pub(crate) fn new(
        path: PathBuf,
        token: String,
        sandbox_id: String,
        generation: String,
        generation_number: u64,
        env: BTreeMap<String, String>,
        e2b: bool,
    ) -> Self {
        Self {
            path,
            token,
            sandbox_id,
            generation,
            generation_number,
            env,
            e2b,
            jobs: Default::default(),
            slots: Arc::new(tokio::sync::Semaphore::new(16)),
            http_slots: Arc::new(tokio::sync::Semaphore::new(64)),
        }
    }
}
#[derive(Clone, Debug)]
enum Event {
    Start,
    Output(u8, Vec<u8>),
    Exit(i32, Option<String>),
}
struct Journal {
    events: Vec<Event>,
    bytes: usize,
    done: bool,
}
struct Job {
    id: String,
    pid: u32,
    generation: u64,
    launch: ExecLaunch,
    tag: Option<String>,
    started: chrono::DateTime<chrono::Utc>,
    journal: Mutex<Journal>,
    changed: watch::Sender<u64>,
    input: AsyncMutex<Option<OwnedWriteHalf>>,
    stdin_closed: AtomicBool,
}
impl Job {
    fn push(&self, event: Event) -> bool {
        let mut journal = self.journal.lock().unwrap();
        if journal.done {
            return false;
        }
        let size = match &event {
            Event::Output(_, bytes) => bytes.len(),
            _ => 0,
        };
        if !matches!(event, Event::Exit(..))
            && (journal.bytes + size > 4 * 1024 * 1024 || journal.events.len() >= 16384)
        {
            return false;
        }
        journal.bytes += size;
        journal.done = matches!(event, Event::Exit(..));
        journal.events.push(event);
        self.changed.send_modify(|revision| *revision += 1);
        true
    }
    fn done(&self) -> bool {
        self.journal.lock().unwrap().done
    }
}
#[derive(Debug)]
struct Error(StatusCode, String);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, [(header::CONNECTION,"close")], axum::Json(json!({"code": match self.0 { StatusCode::NOT_FOUND => "not_found", StatusCode::NOT_IMPLEMENTED => "unimplemented", StatusCode::UNAUTHORIZED => "unauthenticated", _ => "invalid_argument" }, "message": self.1}))).into_response()
    }
}
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self(StatusCode::BAD_GATEWAY, error.to_string())
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error(StatusCode::BAD_REQUEST, message.into())
}
fn unsupported(message: impl Into<String>) -> Error {
    Error(StatusCode::NOT_IMPLEMENTED, message.into())
}
async fn write<T: serde::Serialize>(
    stream: &mut (impl AsyncWriteExt + Unpin),
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "guest frame exceeds limit");
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
async fn read(stream: &mut (impl AsyncReadExt + Unpin)) -> Result<Reply> {
    let length = stream.read_u32().await? as usize;
    ensure!(length <= MAX_FRAME, "oversized guest reply");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn accepted(reply: Reply) -> Result<Reply, Error> {
    match reply {
        Reply::Failure { errno, message } => Err(Error(
            match errno {
                libc::ENOENT | libc::ENOTDIR => StatusCode::NOT_FOUND,
                libc::EACCES | libc::EPERM => StatusCode::FORBIDDEN,
                libc::EEXIST => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            },
            message,
        )),
        Reply::Error { message } => Err(invalid(message)),
        other => Ok(other),
    }
}
impl Service {
    async fn connect(&self, request: &GuestRequest) -> Result<UnixStream> {
        let mut stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match UnixStream::connect(&self.path).await {
                    // Linux AF_UNIX returns EAGAIN when a nonblocking connect
                    // fills the listener's accept queue. No connection/request
                    // exists yet: wait within the original deadline, without
                    // replaying any command or increasing admission limits.
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    result => return result,
                }
            }
        })
        .await??;
        write(&mut stream, request).await?;
        Ok(stream)
    }
    async fn rpc(&self, request: &GuestRequest) -> Result<Reply, Error> {
        let mut stream = self.connect(request).await?;
        accepted(
            tokio::time::timeout(Duration::from_secs(10), read(&mut stream))
                .await
                .map_err(|_| Error(StatusCode::GATEWAY_TIMEOUT, "guest RPC timeout".into()))??,
        )
    }
    async fn info(&self) -> Result<Value, Error> {
        let Reply::Value { value } = self.rpc(&GuestRequest::Info {}).await? else {
            return Err(invalid("invalid guest info reply"));
        };
        if value["sandboxId"] != self.sandbox_id || value["generation"] != self.generation {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "guest generation mismatch".into(),
            ));
        }
        Ok(value)
    }
    async fn cpu_ticks(&self) -> Result<(u64, u64), Error> {
        let value = self.info().await?;
        Ok((
            value["cpu_busy_ticks"].as_u64().unwrap_or(0),
            value["cpu_total_ticks"].as_u64().unwrap_or(0),
        ))
    }
    async fn file(&self, request: FileRequest) -> Result<Value, Error> {
        match self.rpc(&GuestRequest::File { request }).await? {
            Reply::Value { value } => Ok(value),
            _ => Err(invalid("invalid guest file reply")),
        }
    }
    async fn download(&self, path: PathBuf, range: Option<&str>) -> Result<Response, Error> {
        let selection = if let Some(range) = range {
            let info = self.file(FileRequest::Stat { path: path.clone() }).await?;
            let size = info["size"]
                .as_u64()
                .ok_or_else(|| invalid("invalid guest file size"))?;
            let Some((start, length)) = byte_range(range, size) else {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                    Body::empty(),
                )
                    .into_response());
            };
            Some((start, length, size))
        } else {
            None
        };
        let mut stream = self
            .connect(&GuestRequest::File {
                request: FileRequest::Read { path },
            })
            .await?;
        ensure_ready(accepted(read(&mut stream).await?)?)?;
        let (skip, remaining) =
            selection.map_or((0, None), |(start, length, _)| (start, Some(length)));
        let body = futures_util::stream::try_unfold(
            (stream, skip, remaining),
            |(mut stream, mut skip, mut remaining)| async move {
                loop {
                    if remaining == Some(0) {
                        return Ok(None);
                    }
                    match accepted(read(&mut stream).await?).map_err(|e| anyhow::anyhow!(e.1))? {
                        Reply::Output { bytes, .. } => {
                            let discarded = skip.min(bytes.len() as u64) as usize;
                            skip -= discarded as u64;
                            let length = remaining.map_or(bytes.len() - discarded, |left| {
                                left.min((bytes.len() - discarded) as u64) as usize
                            });
                            if length == 0 {
                                continue;
                            }
                            if let Some(left) = remaining.as_mut() {
                                *left -= length as u64;
                            }
                            return Ok(Some((
                                Bytes::from(bytes).slice(discarded..discarded + length),
                                (stream, skip, remaining),
                            )));
                        }
                        Reply::Exit { code: 0 } if remaining.is_none_or(|left| left == 0) => {
                            return Ok(None);
                        }
                        Reply::Exit { code: 0 } => bail!("guest file ended before requested range"),
                        _ => bail!("invalid guest read stream"),
                    }
                }
            },
        );
        let mut response = (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::ACCEPT_RANGES, "bytes"),
            ],
            Body::from_stream(body),
        )
            .into_response();
        if let Some((start, length, size)) = selection {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, length.to_string().parse().unwrap());
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{size}", start + length - 1)
                    .parse()
                    .unwrap(),
            );
        }
        Ok(response)
    }
    async fn upload_begin(&self, path: PathBuf, mode: u32) -> Result<UnixStream, Error> {
        let mut stream = self
            .connect(&GuestRequest::File {
                request: FileRequest::Write { path, mode },
            })
            .await?;
        ensure_ready(accepted(read(&mut stream).await?)?)?;
        Ok(stream)
    }
    async fn upload_end(stream: &mut UnixStream) -> Result<Value, Error> {
        write(stream, &StdinFrame::Eof {}).await?;
        match accepted(read(stream).await?)? {
            Reply::Value { value } => Ok(value),
            _ => Err(invalid("invalid upload completion")),
        }
    }
    fn job(&self, selector: &Value) -> Result<Arc<Job>, Error> {
        if !selector.is_string()
            && !selector.as_object().is_some_and(|object| {
                object.len() == 1
                    && (object
                        .get("pid")
                        .and_then(Value::as_u64)
                        .is_some_and(|pid| pid > 0 && pid <= u64::from(u32::MAX))
                        || object
                            .get("tag")
                            .and_then(Value::as_str)
                            .is_some_and(|tag| !tag.is_empty()))
            })
        {
            return Err(invalid("exactly one process pid or tag required"));
        }
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|job| {
                selector["pid"].as_u64() == Some(u64::from(job.pid))
                    || selector["tag"]
                        .as_str()
                        .is_some_and(|tag| job.tag.as_deref() == Some(tag))
                    || selector.as_str() == Some(job.id.as_str())
            })
            .cloned()
            .ok_or_else(|| Error(StatusCode::NOT_FOUND, "process not found".into()))
    }
    async fn signal(&self, job: &Job, signal: i32) -> Result<(), Error> {
        if job.done() {
            return Err(Error(StatusCode::NOT_FOUND, "process exited".into()));
        }
        ensure_ready(
            self.rpc(&GuestRequest::Signal {
                pid: job.pid,
                generation: job.generation,
                signal,
            })
            .await?,
        )
    }
    async fn start(
        &self,
        launch: ExecLaunch,
        stdin: bool,
        tag: Option<String>,
    ) -> Result<Arc<Job>, Error> {
        let reservation = {
            let mut jobs = self.jobs.lock().unwrap();
            if jobs.iter().filter(|job| !job.done()).count() >= 16 {
                return Err(Error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "native active process limit (16)".into(),
                ));
            }
            while jobs.len() >= 64 {
                let Some(index) = jobs.iter().position(|job| job.done()) else {
                    return Err(Error(
                        StatusCode::TOO_MANY_REQUESTS,
                        "native retained process limit".into(),
                    ));
                };
                jobs.remove(index);
            }
            // A bounded semaphore below also covers concurrent launches before registration.
            self.launch_slot()
        };
        let _reservation = reservation?;
        let mut stream = self
            .connect(&GuestRequest::Start {
                launch: launch.clone(),
            })
            .await?;
        let Reply::Started { pid, generation } = accepted(read(&mut stream).await?)? else {
            return Err(invalid("guest did not accept process"));
        };
        let (reader, writer) = stream.into_split();
        let (changed, _) = watch::channel(0);
        let job = Arc::new(Job {
            id: uuid::Uuid::new_v4().to_string(),
            pid,
            generation,
            launch,
            tag,
            started: chrono::Utc::now(),
            journal: Mutex::new(Journal {
                events: vec![Event::Start],
                bytes: 0,
                done: false,
            }),
            changed,
            input: AsyncMutex::new(Some(writer)),
            stdin_closed: AtomicBool::new(false),
        });
        {
            let mut jobs = self.jobs.lock().unwrap();
            // Recheck after asynchronous spawn: parallel admissions may have
            // consumed the retention slots observed before the await.
            while jobs.len() >= 64 {
                let index = jobs
                    .iter()
                    .position(|job| job.done())
                    .expect("at most 16 active execs");
                jobs.remove(index);
            }
            jobs.push_back(job.clone());
        }
        let current = job.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let mut reader = reader;
            let result: Result<()> = async {
                loop {
                    match read(&mut reader).await? {
                        Reply::Output { stream, bytes } => {
                            if !current.push(Event::Output(stream, bytes)) {
                                bail!("native output limit exceeded (4 MiB)");
                            }
                        }
                        Reply::Exit { code } => {
                            current.push(Event::Exit(code, None));
                            break;
                        }
                        other => {
                            accepted(other).map_err(|e| anyhow::anyhow!(e.1))?;
                            bail!("invalid exec stream");
                        }
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                let _ = service.signal(&current, libc::SIGKILL).await;
                current.push(Event::Exit(125, Some(error.to_string())));
            }
            current.input.lock().await.take();
            drop(_reservation);
        });
        if !stdin {
            job.close_stdin().await?;
        }
        Ok(job)
    }
    fn launch_slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Error> {
        // Per-service limit; socket slots remain available for signal/files/info.
        self.slots.clone().try_acquire_owned().map_err(|_| {
            Error(
                StatusCode::TOO_MANY_REQUESTS,
                "native active process limit (16)".into(),
            )
        })
    }
}
impl Job {
    async fn close_stdin(&self) -> Result<(), Error> {
        let mut input = self.input.lock().await;
        if !self.stdin_closed.load(Ordering::Acquire) {
            if let Some(stream) = input.as_mut() {
                write(stream, &StdinFrame::Eof {}).await?;
            }
            self.stdin_closed.store(true, Ordering::Release);
        }
        // Keep the transport's write half alive until Exit: EOF is a protocol
        // frame, not a vsock half-close (which can reset the duplex stream).
        Ok(())
    }
    async fn send_input(&self, bytes: &[u8]) -> Result<(), Error> {
        let mut input = self.input.lock().await;
        if self.stdin_closed.load(Ordering::Acquire) {
            return Err(invalid("process stdin is closed"));
        }
        let stream = input
            .as_mut()
            .ok_or_else(|| invalid("process stdin is closed"))?;
        for bytes in bytes.chunks(16384) {
            write(
                stream,
                &StdinFrame::Data {
                    bytes: bytes.to_vec(),
                },
            )
            .await?;
        }
        Ok(())
    }
}
fn ensure_ready(reply: Reply) -> Result<(), Error> {
    if matches!(reply, Reply::Ready) {
        Ok(())
    } else {
        Err(invalid("invalid guest acknowledgement"))
    }
}
async fn json_body(request: Request) -> Result<Value, Error> {
    let bytes = axum::body::to_bytes(request.into_body(), MAX_FRAME)
        .await
        .map_err(|_| invalid("JSON body exceeds limit"))?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
}
fn query(request: &Request, key: &str) -> Option<String> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .find_map(|(k, v)| (k == key).then(|| v.into_owned()))
}
fn timestamp() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn guest_path(value: &str, e2b: bool) -> Result<PathBuf, Error> {
    let path = if value == "~" {
        "/root".into()
    } else if let Some(rest) = value.strip_prefix("~/") {
        format!("/root/{rest}")
    } else if e2b && !value.starts_with('/') {
        format!("/root/{value}")
    } else {
        value.to_owned()
    };
    if !path.starts_with('/') || path.contains('\0') {
        return Err(invalid("absolute guest path required"));
    }
    Ok(path.into())
}
pub(crate) async fn serve(listener: TcpListener, service: Service) -> Result<()> {
    let router = Router::new()
        .fallback(any(move |request: Request| {
            let service = service.clone();
            async move {
                let permit = service
                    .http_slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        Error(
                            StatusCode::TOO_MANY_REQUESTS,
                            "native HTTP stream limit (64)".into(),
                        )
                    })?;
                let name = if service.e2b {
                    "x-access-token"
                } else {
                    "x-execd-access-token"
                };
                if request.headers().get_all(name).iter().count() != 1
                    || !request
                        .headers()
                        .get(name)
                        .and_then(|h| h.to_str().ok())
                        .is_some_and(|value| {
                            crate::runtime::constant_time_equal(value, &service.token)
                        })
                {
                    return Err(Error(
                        StatusCode::UNAUTHORIZED,
                        "invalid sandbox credential".into(),
                    ));
                }
                if request.headers().contains_key(header::UPGRADE)
                    || request.method() == axum::http::Method::CONNECT
                {
                    return Err(unsupported("upgrade/tunnel is unsupported"));
                }
                let response = if service.e2b {
                    e2b::handle(service, request).await
                } else {
                    opensandbox::handle(service, request).await
                }?;
                use futures_util::StreamExt;
                Ok(response.map(|body| {
                    Body::from_stream(futures_util::stream::unfold(
                        (body.into_data_stream(), permit),
                        |(mut body, permit)| async move {
                            body.next().await.map(|chunk| (chunk, (body, permit)))
                        },
                    ))
                }))
            }
        }))
        .layer(DefaultBodyLimit::disable());
    use axum::serve::ListenerExt;
    // SSE/Connect send admission and output separately; delayed ACK must not
    // hold a small completion frame behind an earlier unacknowledged packet.
    axum::serve(
        listener.tap_io(|stream| {
            let _ = stream.set_nodelay(true);
        }),
        router,
    )
    .await?;
    Ok(())
}

/// A single inclusive byte range, without integer wraparound.
fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    fn number(value: &str) -> Option<u64> {
        let value = value.trim();
        (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse().ok())
            .flatten()
    }
    let (start, end) = value.strip_prefix("bytes=")?.trim().split_once('-')?;
    if size == 0 {
        return None;
    }
    if start.trim().is_empty() {
        let length = number(end)?.min(size);
        return (length > 0).then_some((size - length, length));
    }
    let start = number(start)?;
    if start >= size {
        return None;
    }
    let end = if end.trim().is_empty() {
        size - 1
    } else {
        number(end)?.min(size - 1)
    };
    (end >= start).then(|| (start, end - start + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn guest_connect_waits_for_a_full_unix_accept_queue_before_sending_once() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backlog.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        // Change only this owned listener's backlog, not a system-wide limit.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let mut fillers = Vec::new();
        loop {
            match UnixStream::connect(&path).await {
                Ok(stream) => fillers.push(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("unexpected backlog result: {other:?}"),
            }
            assert!(fillers.len() <= 4);
        }
        let service = Service::new(
            path,
            "secret".into(),
            "owner".into(),
            "generation".into(),
            1,
            BTreeMap::new(),
            false,
        );
        let pending = tokio::spawn(async move { service.connect(&GuestRequest::Ping {}).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !pending.is_finished(),
            "valid admitted request failed immediately on a full accept queue"
        );
        for _ in 0..fillers.len() {
            let _accepted = listener.accept().await.unwrap();
        }
        let (mut accepted, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut connected = pending.await.unwrap().unwrap();
        let length = accepted.read_u32().await.unwrap();
        let mut bytes = vec![0; length as usize];
        accepted.read_exact(&mut bytes).await.unwrap();
        assert!(matches!(
            serde_json::from_slice::<GuestRequest>(&bytes).unwrap(),
            GuestRequest::Ping {}
        ));
        write(&mut accepted, &Reply::Ready).await.unwrap();
        assert!(matches!(read(&mut connected).await.unwrap(), Reply::Ready));
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn guest_connect_backpressure_keeps_deadline_and_permanent_errors() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().unwrap();
        let missing = Service::new(
            directory.path().join("missing.sock"),
            "secret".into(),
            "owner".into(),
            "generation".into(),
            1,
            BTreeMap::new(),
            false,
        );
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            missing.connect(&GuestRequest::Ping {}),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        let path = directory.path().join("full.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let mut fillers = Vec::new();
        loop {
            match UnixStream::connect(&path).await {
                Ok(stream) => fillers.push(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("unexpected backlog result: {other:?}"),
            }
            assert!(fillers.len() <= 4);
        }
        let full = Service::new(
            path,
            "secret".into(),
            "owner".into(),
            "generation".into(),
            1,
            BTreeMap::new(),
            false,
        );
        let error = full.connect(&GuestRequest::Ping {}).await.err().unwrap();
        assert!(error.is::<tokio::time::error::Elapsed>(), "{error:#}");
        for _ in 0..fillers.len() {
            let _accepted = listener.accept().await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "timed-out request left a queued connection"
        );
    }

    #[tokio::test]
    async fn downloads_honor_byte_ranges_across_guest_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest.sock");
        let unix = tokio::net::UnixListener::bind(&path).unwrap();
        let bytes = Arc::new((0..=255).cycle().take(40003).collect::<Vec<u8>>());
        let source = bytes.clone();
        let peer = tokio::spawn(async move {
            loop {
                let (mut stream, _) = unix.accept().await.unwrap();
                let bytes = source.clone();
                tokio::spawn(async move {
                    let size = stream.read_u32().await.unwrap();
                    let mut request = vec![0; size as usize];
                    stream.read_exact(&mut request).await.unwrap();
                    match serde_json::from_slice::<GuestRequest>(&request).unwrap() {
                        GuestRequest::File {
                            request: FileRequest::Stat { .. },
                        } => {
                            write(
                                &mut stream,
                                &Reply::Value {
                                    value: json!({"size":bytes.len()}),
                                },
                            )
                            .await
                            .unwrap();
                        }
                        GuestRequest::File {
                            request: FileRequest::Read { .. },
                        } => {
                            write(&mut stream, &Reply::Ready).await.unwrap();
                            for chunk in bytes.chunks(16384) {
                                if write(
                                    &mut stream,
                                    &Reply::Output {
                                        stream: 1,
                                        bytes: chunk.to_vec(),
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    return; // A completed range closes its guest read stream.
                                }
                            }
                            let _ = write(&mut stream, &Reply::Exit { code: 0 }).await;
                        }
                        _ => panic!("unexpected guest operation"),
                    }
                });
            }
        });
        let service = Service::new(
            path,
            "secret".into(),
            "owner".into(),
            "generation".into(),
            1,
            BTreeMap::new(),
            false,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!(
            "http://{}/files/download?path=/data",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(serve(listener, service));
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for (range, start, end) in [
            ("bytes=16380-32772", 16380, 32773),
            ("bytes=-17", 39986, 40003),
            ("bytes=39990-", 39990, 40003),
            ("bytes=0-999999", 0, 40003),
        ] {
            let response = client
                .get(&address)
                .header("x-execd-access-token", "secret")
                .header(header::RANGE, range)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                (end - start).to_string()
            );
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                format!("bytes {start}-{}/40003", end - 1)
            );
            assert_eq!(response.bytes().await.unwrap().as_ref(), &bytes[start..end]);
        }
        for range in [
            "bytes=40003-",
            "bytes=-0",
            "bytes=5-4",
            "bytes=foo",
            "bytes=0-1,3-4",
        ] {
            let response = client
                .get(&address)
                .header("x-execd-access-token", "secret")
                .header(header::RANGE, range)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */40003");
        }
        let response = client
            .get(format!("{address}&offset=2&limit=1"))
            .header("x-execd-access-token", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let response = client
            .get(address)
            .header("x-execd-access-token", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
        assert_eq!(byte_range("bytes=0-", 0), None);
        assert_eq!(byte_range("bytes=0-18446744073709551616", 10), None);
        server.abort();
        peer.abort();
    }

    #[tokio::test]
    async fn mapping_authenticates_before_rpc_and_rejects_foreign_guest_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest.sock");
        let unix = tokio::net::UnixListener::bind(&path).unwrap();
        let service = Service::new(
            path,
            "secret".into(),
            "sb-owner".into(),
            "generation".into(),
            7,
            BTreeMap::new(),
            false,
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve(listener, service));
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            client
                .get(format!("{address}/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), unix.accept())
                .await
                .is_err()
        );
        let peer = tokio::spawn(async move {
            let (mut stream, _) = unix.accept().await.unwrap();
            let size = stream.read_u32().await.unwrap();
            let mut request = vec![0; size as usize];
            stream.read_exact(&mut request).await.unwrap();
            assert!(matches!(
                serde_json::from_slice::<GuestRequest>(&request).unwrap(),
                GuestRequest::Info {}
            ));
            write(
                &mut stream,
                &Reply::Value {
                    value: json!({"sandboxId":"sb-owner","generation":"other"}),
                },
            )
            .await
            .unwrap();
        });
        let response = client
            .get(format!("{address}/ready"))
            .header("x-execd-access-token", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        peer.await.unwrap();
        server.abort();
    }
    #[test]
    fn terminal_event_is_retained_when_output_budget_is_exhausted() {
        let (changed, _) = watch::channel(0);
        let job = Job {
            id: "id".into(),
            pid: 2,
            generation: 1,
            launch: ExecLaunch {
                argv: vec!["tool".into()],
                env: BTreeMap::new(),
                cwd: None,
                user: None,
                tty: None,
            },
            tag: None,
            started: chrono::Utc::now(),
            journal: Mutex::new(Journal {
                events: vec![],
                bytes: 4 * 1024 * 1024,
                done: false,
            }),
            changed,
            input: AsyncMutex::new(None),
            stdin_closed: AtomicBool::new(false),
        };
        assert!(!job.push(Event::Output(1, vec![0])));
        assert!(job.push(Event::Exit(125, Some("output limit".into()))));
        assert!(job.done());
        assert!(!job.push(Event::Output(1, vec![])));
    }
}

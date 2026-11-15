//! Async framing over the same session authority engine as the blocking adapter.
//! Idle sockets and disconnected peer leases consume futures, never worker threads.
use super::*;
use std::{
    future::Future,
    os::fd::{FromRawFd, OwnedFd},
    pin::Pin,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, Interest, unix::AsyncFd},
    net::UnixStream as AsyncStream,
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
};
use tokio_util::sync::CancellationToken;

const WORKERS: usize = 4;
const IN_FLIGHT_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Work {
    operations: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Work {
    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let permit = self
            .operations
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("pool worker admission closed"))?;
        // The permit lives in the job, not its awaiting future. Cancellation
        // cannot admit extra blocking jobs while an earlier job is still running.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// A bounded asynchronous pool service. Create one per pool, not per socket.
///
/// Connections retain the existing per-session reference limit. At most four
/// blocking operations and 32 MiB of request/reply buffers are admitted across
/// the service. Body reads/writes have a five-second absolute request deadline;
/// idle sockets have no expiry. Requires an active Tokio I/O/time runtime.
/// Constructors reject zero reference limits and connection limits outside
/// 1..=65536 before admitting any socket or changing pool contents.
///
/// Dropping a connection future closes its transport, not its mapped references.
/// A separate asynchronous peer watcher retains those until confirmed process
/// exit. Unexpected runtime cancellation conservatively retains uncertain pins
/// and their admission permit until owner exit rather than recycling live RAM.
pub struct AsyncPoolService {
    pool: Owner,
    registration: Registration,
    connections: Arc<Semaphore>,
    max_references: usize,
    work: Work,
}

impl AsyncPoolService {
    /// Serve the legacy compressed backend. Transport loss may release its
    /// references immediately because this mode does not grant mapped backing.
    pub fn compressed(
        pool: Arc<Mutex<CompressedPool>>,
        max_connections: usize,
        max_references: usize,
    ) -> io::Result<Self> {
        Self::new(
            Owner::Compressed(pool),
            Registration::Legacy,
            max_connections,
            max_references,
        )
    }

    /// Serve physical pages with unchanged batch discovery and FD contracts.
    /// A generation requires v4 witness registration; None retains legacy v3.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn shared(
        pool: Arc<Mutex<super::super::shared::SharedPool>>,
        generation: Option<Arc<super::super::generation::PhysicalGeneration>>,
        max_connections: usize,
        max_references: usize,
    ) -> io::Result<Self> {
        Self::new(
            Owner::Shared(pool),
            generation.map_or(Registration::Legacy, Registration::Required),
            max_connections,
            max_references,
        )
    }

    /// Serve Linux sealed compressed objects, retaining disconnected references
    /// until pidfd-confirmed process exit, exactly like the synchronous adapter.
    #[cfg(target_os = "linux")]
    pub fn sealed(
        pool: Arc<Mutex<super::super::sealed::SealedPool>>,
        max_connections: usize,
        max_references: usize,
    ) -> io::Result<Self> {
        Self::new(
            Owner::Sealed(pool),
            Registration::Legacy,
            max_connections,
            max_references,
        )
    }

    fn new(
        pool: Owner,
        registration: Registration,
        connections: usize,
        references: usize,
    ) -> io::Result<Self> {
        if connections == 0 || connections > 65536 || references == 0 {
            return Err(invalid("invalid asynchronous pool limits"));
        }
        Ok(Self {
            pool,
            registration,
            connections: Arc::new(Semaphore::new(connections)),
            max_references: references,
            work: Work {
                operations: Arc::new(Semaphore::new(WORKERS)),
                bytes: Arc::new(Semaphore::new(IN_FLIGHT_BYTES)),
            },
        })
    }

    /// Authenticate and admit a socket before spawning its connection future.
    /// WouldBlock rejects excess connections without allocating a session.
    /// The returned future owns the socket; cancellation notifies its reaper.
    /// Disconnected sessions with mapped references still occupy admission
    /// until peer exit. No reference is released merely because a deadline fires.
    pub fn connection(
        &self,
        stream: AsyncStream,
    ) -> io::Result<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>> {
        let admission = self.connections.clone().try_acquire_owned().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "pool connection budget exhausted",
            )
        })?;
        let stream = stream.into_std()?;
        crate::host_transport::authorize_host_peer_sync(&stream).map_err(io::Error::other)?;
        let peer = if self.pool.requires_peer_exit() {
            Some(AsyncFd::new(super::super::shared::peer_pidfd(&stream)?)?)
        } else {
            None
        };
        let stream = AsyncStream::from_std(stream)?;
        let mut session = Session::new(
            self.pool.clone(),
            self.max_references,
            self.registration.clone(),
        )?;
        session.admission = Some(admission);
        let state = Arc::new(Mutex::new(session));
        let (closed, receiver) = oneshot::channel::<()>();
        let cancelled = CancellationToken::new();
        let work = self.work.clone();
        let reaper_state = state.clone();
        let reaper_cancel = cancelled.clone();
        let reaper_work = work.clone();
        tokio::spawn(async move {
            if let Err(error) = reap(reaper_state, peer, receiver, reaper_work).await {
                eprintln!("memory-pool peer cleanup: {error}; uncertain pins retained");
            }
            reaper_cancel.cancel();
        });
        Ok(Box::pin(async move {
            // This sender is captured even before the future is first polled.
            // Drop/cancellation always wakes the independent lifecycle task.
            let _closed = closed;
            tokio::select! {
                result = serve(stream, state, work) => result,
                _ = cancelled.cancelled() => Err(io::Error::new(io::ErrorKind::NotConnected, "pool peer exited")),
            }
        }))
    }
}

struct Exchange {
    input: io::Cursor<Vec<u8>>,
    output: Vec<u8>,
    descriptor: Option<(usize, OwnedFd)>,
}

impl Read for Exchange {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        Read::read(&mut self.input, bytes)
    }
}
impl Write for Exchange {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .output
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_BATCH_REPLY + 5)
        {
            return Err(invalid("pool response exceeds frame budget"));
        }
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl ServerIo for Exchange {
    fn mapping(&mut self, fd: std::os::fd::RawFd) -> io::Result<()> {
        if self.descriptor.is_some() {
            return Err(invalid("multiple descriptors in pool response"));
        }
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        self.descriptor = Some((self.output.len(), unsafe { OwnedFd::from_raw_fd(copy) }));
        Ok(())
    }
}

fn timed_out(_: tokio::time::error::Elapsed) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "pool request deadline exceeded")
}

async fn frame(
    stream: &mut AsyncStream,
    work: &Work,
) -> io::Result<Option<(Exchange, OwnedSemaphorePermit, tokio::time::Instant)>> {
    let mut op = [0];
    // Idle is free of workers, buffer permits and timers.
    if stream.read(&mut op).await? == 0 {
        return Ok(None);
    }
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let mut header = [0; 4];
    let length = match op[0] {
        PUT | PUT_SHARED | PUT_DUPLICATE | PUT_SEALED | PUT_BATCH | RELEASE_BATCH
        | DISCOVER_XXH3 => {
            tokio::time::timeout_at(deadline, stream.read_exact(&mut header))
                .await
                .map_err(timed_out)??;
            let count = u32::from_le_bytes(header) as usize;
            match op[0] {
                PUT | PUT_SHARED | PUT_DUPLICATE | PUT_SEALED
                    if count > 0 && count <= BLOCK_BYTES =>
                {
                    count
                }
                PUT_BATCH if count > 0 && count <= DISCOVERY_BATCH => {
                    count * super::super::shared::PAGE
                }
                RELEASE_BATCH if count > 0 && count <= DISCOVERY_BATCH => count * 8,
                DISCOVER_XXH3 if count > 0 && count <= DISCOVERY_BATCH => count * 24,
                _ => return Err(invalid("invalid pool request size")),
            }
        }
        GET | RELEASE => 8,
        REGISTER_GENERATION => 16,
        STATS | SHARED_STATS | MAPPING | SEALED | DISCOVERY => 0,
        _ => return Err(invalid("unknown pool operation")),
    };
    let prefixed = matches!(
        op[0],
        PUT | PUT_SHARED | PUT_DUPLICATE | PUT_SEALED | PUT_BATCH | RELEASE_BATCH | DISCOVER_XXH3
    );
    let total = 1 + if prefixed { 4 } else { 0 } + length;
    let charge = total + MAX_BATCH_REPLY + 5;
    let permit = tokio::time::timeout_at(
        deadline,
        work.bytes.clone().acquire_many_owned(charge as u32),
    )
    .await
    .map_err(timed_out)?
    .map_err(|_| io::Error::other("pool frame admission closed"))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(total).map_err(io::Error::other)?;
    bytes.push(op[0]);
    if prefixed {
        bytes.extend_from_slice(&header);
    }
    let start = bytes.len();
    bytes.resize(total, 0);
    tokio::time::timeout_at(deadline, stream.read_exact(&mut bytes[start..]))
        .await
        .map_err(timed_out)??;
    let mut output = Vec::new();
    // Reserve exactly the charged reply ceiling. Geometric Vec growth could
    // otherwise retain more capacity than the global frame permit accounts for.
    output
        .try_reserve_exact(MAX_BATCH_REPLY + 5)
        .map_err(io::Error::other)?;
    Ok(Some((
        Exchange {
            input: io::Cursor::new(bytes),
            output,
            descriptor: None,
        },
        permit,
        deadline,
    )))
}

async fn serve(mut stream: AsyncStream, state: Arc<Mutex<Session>>, work: Work) -> io::Result<()> {
    let id = state
        .lock()
        .map_err(|_| io::Error::other("session poisoned"))?
        .id;
    let mut hello = MAGIC.to_vec();
    hello.extend_from_slice(&id);
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(&hello))
        .await
        .map_err(timed_out)??;
    while let Some((mut exchange, buffers, deadline)) = frame(&mut stream, &work).await? {
        let session = state.clone();
        let reply = tokio::time::timeout_at(
            deadline,
            work.run(move || {
                // A timed-out waiter must not release the memory charge while its
                // detached blocking job still owns the frame and response buffers.
                session
                    .lock()
                    .map_err(|_| io::Error::other("session poisoned"))?
                    .exchange(&mut exchange)?;
                Ok((exchange, buffers))
            }),
        )
        .await
        .map_err(timed_out)??;
        let (reply, _buffers) = reply;
        if let Some((position, descriptor)) = reply.descriptor {
            tokio::time::timeout_at(deadline, stream.write_all(&reply.output[..position]))
                .await
                .map_err(timed_out)??;
            tokio::time::timeout_at(
                deadline,
                stream.async_io(Interest::WRITABLE, || {
                    // AsyncStream and the borrowed standard view share one fd; never
                    // construct an owning File/UnixStream that could close it.
                    let borrowed = std::mem::ManuallyDrop::new(unsafe {
                        <UnixStream as std::os::fd::FromRawFd>::from_raw_fd(stream.as_raw_fd())
                    });
                    send_mapping_fd(&borrowed, descriptor.as_raw_fd())
                }),
            )
            .await
            .map_err(timed_out)??;
            tokio::time::timeout_at(deadline, stream.write_all(&reply.output[position..]))
                .await
                .map_err(timed_out)??;
        } else {
            tokio::time::timeout_at(deadline, stream.write_all(&reply.output))
                .await
                .map_err(timed_out)??;
        }
    }
    Ok(())
}

async fn peer_exit(peer: &AsyncFd<OwnedFd>) -> io::Result<()> {
    loop {
        let mut ready = peer.readable().await?;
        #[cfg(target_os = "linux")]
        {
            let mut poll = libc::pollfd {
                fd: peer.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            if poll.revents & libc::POLLIN != 0 {
                return Ok(());
            }
        }
        #[cfg(target_os = "macos")]
        {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let count = unsafe {
                libc::kevent(
                    peer.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 1
                && event.flags & libc::EV_ERROR == 0
                && event.fflags & libc::NOTE_EXIT != 0
            {
                return Ok(());
            }
        }
        ready.clear_ready();
    }
}

async fn reap(
    state: Arc<Mutex<Session>>,
    peer: Option<AsyncFd<OwnedFd>>,
    mut closed: oneshot::Receiver<()>,
    work: Work,
) -> io::Result<()> {
    let exited = if let Some(peer) = &peer {
        tokio::select! { _ = &mut closed => false, result = peer_exit(peer) => { result?; true } }
    } else {
        let _ = closed.await;
        false
    };
    let disconnected = state.clone();
    let pinned = work
        .run(move || {
            let mut session = disconnected
                .lock()
                .map_err(|_| io::Error::other("session poisoned"))?;
            session.disconnect()?;
            Ok(!session.references.is_empty())
        })
        .await?;
    if pinned
        && !exited
        && let Some(peer) = &peer
    {
        peer_exit(peer).await?;
    }
    work.run(move || {
        state
            .lock()
            .map_err(|_| io::Error::other("session poisoned"))?
            .retire()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> Work {
        Work {
            operations: Arc::new(Semaphore::new(WORKERS)),
            bytes: Arc::new(Semaphore::new(IN_FLIGHT_BYTES)),
        }
    }

    #[tokio::test]
    async fn cancelled_blocking_work_keeps_its_job_and_buffer_admission_until_exit() {
        let work = work();
        let buffers = work.bytes.clone().acquire_many_owned(4096).await.unwrap();
        let (started, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let running_work = work.clone();
        let task = tokio::spawn(async move {
            running_work
                .run(move || {
                    let _buffers = buffers;
                    let _ = started.send(());
                    wait.recv().map_err(io::Error::other)?;
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(work.operations.available_permits(), WORKERS - 1);
        assert_eq!(work.bytes.available_permits(), IN_FLIGHT_BYTES - 4096);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while work.operations.available_permits() != WORKERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(work.bytes.available_permits(), IN_FLIGHT_BYTES);
    }

    #[tokio::test]
    async fn cancellation_before_first_poll_reaps_empty_session_and_admission() {
        let pool = Arc::new(Mutex::new(
            super::super::super::shared::SharedPool::new(super::super::super::shared::PAGE, 1)
                .unwrap(),
        ));
        let service = AsyncPoolService::shared(pool, None, 1, 1).unwrap();
        let (_client, server) = AsyncStream::pair().unwrap();
        let connection = service.connection(server).unwrap();
        assert_eq!(service.connections.available_permits(), 0);
        let (_excess, server) = AsyncStream::pair().unwrap();
        assert!(
            matches!(service.connection(server), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        drop(connection);
        tokio::time::timeout(Duration::from_secs(2), async {
            while service.connections.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn async_request_deadline_is_absolute_and_idle_connections_are_free() {
        let work = work();
        let task_work = work.clone();
        let (mut client, mut server) = AsyncStream::pair().unwrap();
        let task = tokio::spawn(async move { frame(&mut server, &task_work).await });
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(!task.is_finished(), "idle has no deadline");
        assert_eq!(work.operations.available_permits(), WORKERS);
        assert_eq!(work.bytes.available_permits(), IN_FLIGHT_BYTES);
        client.write_all(&[PUT, 4, 0, 0, 0, 0x55]).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        client.write_all(&[0x55]).await.unwrap();
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(2)).await;
        let result = task.await.unwrap();
        assert!(
            matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut),
            "trickling bytes must not renew a request's deadline"
        );
        assert_eq!(work.bytes.available_permits(), IN_FLIGHT_BYTES);
        assert_eq!(work.operations.available_permits(), WORKERS);
    }

    #[tokio::test]
    async fn async_frame_limits_reject_before_allocating_payload_or_worker() {
        for op in [PUT, PUT_BATCH, RELEASE_BATCH, DISCOVER_XXH3] {
            let work = work();
            let (mut client, mut server) = AsyncStream::pair().unwrap();
            let mut request = vec![op];
            request.extend_from_slice(&u32::MAX.to_le_bytes());
            client.write_all(&request).await.unwrap();
            assert!(frame(&mut server, &work).await.is_err());
            assert_eq!(work.bytes.available_permits(), IN_FLIGHT_BYTES);
            assert_eq!(work.operations.available_permits(), WORKERS);
        }
    }
}

//! Bounded, host-owned write staging. No guest buffer or RAM lease enters this queue.
//!
//! The caller reserves before copying guest bytes and supplies a target retaining
//! its native handle and file-I/O lease. Ordering against reads, metadata changes,
//! namespace mutations and synchronous writes belongs to the caller's operation
//! lock: `flush` is an accepted-prefix barrier, not an operation lock or fsync.
//! Targets must not call back into this buffer, because its worker is singular.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_JOBS: usize = 1024;
const BATCH_BYTES: usize = 1024 * 1024;
const BATCH_CHUNKS: usize = 64;
const BATCH_DELAY: Duration = Duration::from_millis(10);

/// Equal keys promise the same opened object and identical write semantics.
/// The boolean distinguishes kill-privilege handling. The first target executes
/// the vectored write; every target in the batch stays alive through execution.
pub(super) trait Target: Send + 'static {
    fn merge_key(&self) -> (u64, bool);
    fn write(&self, chunks: &[&[u8]], offset: u64) -> io::Result<usize>;
}

#[derive(Clone)]
struct Failure {
    kind: io::ErrorKind,
    raw: Option<i32>,
    message: String,
}

impl Failure {
    fn capture(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw: error.raw_os_error(),
            message: error.to_string(),
        }
    }

    fn error(&self) -> io::Error {
        match self.raw {
            Some(raw) => io::Error::from_raw_os_error(raw),
            None => io::Error::new(self.kind, self.message.clone()),
        }
    }
}

struct Job {
    sequence: u64,
    key: (u64, bool),
    target: Box<dyn Target>,
    bytes: Vec<u8>,
    offset: u64,
    accepted_at: Instant,
}

struct State {
    queue: VecDeque<Job>,
    // Includes reservations, queued jobs and the currently executing batch.
    bytes: usize,
    jobs: usize,
    inflight_bytes: usize,
    inflight_jobs: usize,
    accepted: u64,
    completed: u64,
    accepting: bool,
    exited: bool,
    failure: Option<Failure>,
    // Controls do not consume the data budget. A cut prevents a later write
    // joining its batch, so continuous producers cannot starve a barrier.
    cuts: BTreeMap<u64, usize>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    max_bytes: usize,
    max_jobs: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Worker unwinding is handled by worker_entry, which records a sticky
        // error and wakes every waiter, including after a poisoned lock.
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed
            .wait(state)
            .unwrap_or_else(|error| error.into_inner())
    }

    fn wait_for<'a>(
        &self,
        state: MutexGuard<'a, State>,
        duration: Duration,
    ) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, duration)
            .unwrap_or_else(|error| error.into_inner())
            .0
    }
}

/// A session-local FIFO. Stop is permanent; a new INIT creates a new owner.
pub(super) struct WriteBuffer {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// One byte-and-job reservation, acquired before allocating/copying guest bytes.
/// Dropping it without submission refunds both budgets. Stop invalidates pending
/// permits and drains accepted jobs without waiting for a caller still copying.
pub(super) struct Permit {
    shared: Arc<Shared>,
    size: usize,
    held: bool,
}

impl WriteBuffer {
    pub(super) fn new() -> io::Result<Self> {
        Self::with_limits(DEFAULT_BYTES, DEFAULT_JOBS)
    }

    fn with_limits(max_bytes: usize, max_jobs: usize) -> io::Result<Self> {
        if max_bytes == 0 || max_jobs == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write-buffer budgets must be nonzero",
            ));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                bytes: 0,
                jobs: 0,
                inflight_bytes: 0,
                inflight_jobs: 0,
                accepted: 0,
                completed: 0,
                accepting: true,
                exited: false,
                failure: None,
                cuts: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            max_bytes,
            max_jobs,
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("pvisor-fs-write-buffer".into())
            .spawn(move || worker_entry(worker_shared))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Waits for space without holding the queue mutex or a native write lock.
    /// The caller may still hold its file-I/O and current virtqueue RAM leases;
    /// backpressure does not promise that an outstanding guest request can drain
    /// for VM offload. Do not hold a lock needed by queued targets to progress.
    /// Requests larger than the entire budget fail instead of waiting forever.
    pub(super) fn reserve(&self, size: usize) -> io::Result<Permit> {
        if size > self.shared.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write exceeds the complete write-buffer byte budget",
            ));
        }
        let mut state = self.shared.lock();
        loop {
            admit(&state)?;
            if state.jobs < self.shared.max_jobs && size <= self.shared.max_bytes - state.bytes {
                state.bytes += size;
                state.jobs += 1;
                return Ok(Permit {
                    shared: self.shared.clone(),
                    size,
                    held: true,
                });
            }
            state = self.shared.wait(state);
        }
    }

    /// Waits for all writes accepted before this call, including an in-flight
    /// batch. Later submissions do not extend the cut. This is not fdatasync.
    /// Any session error remains sticky and is returned after the cut drains.
    pub(super) fn flush(&self) -> io::Result<()> {
        let mut state = self.shared.lock();
        let cut = state.accepted;
        if state.completed == cut {
            // Metadata and read-only workloads usually have no pending writes.
            // Avoid allocating a cut node or waking the worker in that case.
            return current_error(&state);
        }
        *state.cuts.entry(cut).or_default() += 1;
        self.shared.changed.notify_all();
        while state.completed < cut && !state.exited {
            state = self.shared.wait(state);
        }
        let count = state.cuts.get_mut(&cut).unwrap();
        *count -= 1;
        if *count == 0 {
            state.cuts.remove(&cut);
        }
        self.shared.changed.notify_all();
        current_error(&state)
    }

    pub(super) fn check_error(&self) -> io::Result<()> {
        current_error(&self.shared.lock())
    }

    /// Permanently rejects new work, drains accepted work, then joins once.
    /// Concurrent callers wait for the same join; errors are never cleared.
    pub(super) fn stop(&self) -> io::Result<()> {
        {
            let mut state = self.shared.lock();
            state.accepting = false;
            self.shared.changed.notify_all();
        }
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(worker) = worker.take() {
            if worker.join().is_err() {
                fail_worker(&self.shared);
            }
        }
        self.check_error()
    }
}

impl Drop for WriteBuffer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl Permit {
    /// `bytes` must use no more allocation capacity than was reserved. Allocate
    /// the exact guest-write length after reservation (for example vec![0; n]).
    /// A rejected submission consumes no sequence and refunds its reservation.
    pub(super) fn submit(
        mut self,
        target: Box<dyn Target>,
        bytes: Vec<u8>,
        offset: u64,
    ) -> io::Result<u64> {
        if bytes.len() != self.size || bytes.capacity() > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write allocation does not match its reserved byte budget",
            ));
        }
        if u64::try_from(bytes.len())
            .ok()
            .and_then(|length| offset.checked_add(length))
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write offset overflows",
            ));
        }
        let key = target.merge_key();
        let mut state = self.shared.lock();
        admit(&state)?;
        let sequence = state.accepted.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write sequence exhausted")
        })?;
        state.queue.push_back(Job {
            sequence,
            key,
            target,
            bytes,
            offset,
            accepted_at: Instant::now(),
        });
        state.accepted = sequence;
        self.held = false;
        self.shared.changed.notify_all();
        Ok(sequence)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.held {
            let mut state = self.shared.lock();
            state.bytes -= self.size;
            state.jobs -= 1;
            self.shared.changed.notify_all();
        }
    }
}

fn current_error(state: &State) -> io::Result<()> {
    match &state.failure {
        Some(error) => Err(error.error()),
        None => Ok(()),
    }
}

fn admit(state: &State) -> io::Result<()> {
    current_error(state)?;
    if !state.accepting || state.exited {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "write-buffer session is stopped",
        ));
    }
    Ok(())
}

// The oldest unresolved cut also seals a merge batch. Completed cuts may stay
// briefly registered until their waiter is scheduled; they do not stall work.
fn pending_cut(state: &State) -> Option<u64> {
    state
        .cuts
        .keys()
        .copied()
        .find(|cut| *cut > state.completed)
}

fn batch_shape(state: &State) -> (usize, usize, bool) {
    let first = state.queue.front().unwrap();
    let cut = pending_cut(state);
    let mut bytes = 0;
    let mut count = 0;
    let mut next_offset = first.offset;
    let mut sealed = !state.accepting || cut.is_some();
    for job in &state.queue {
        if (count != 0
            && (job.key != first.key
                || job.offset != next_offset
                || bytes + job.bytes.len() > BATCH_BYTES))
            || count == BATCH_CHUNKS
            || cut.is_some_and(|cut| job.sequence > cut)
        {
            sealed = true;
            break;
        }
        count += 1;
        bytes += job.bytes.len();
        next_offset += job.bytes.len() as u64;
        if bytes >= BATCH_BYTES || count == BATCH_CHUNKS {
            sealed = true;
            break;
        }
    }
    (count, bytes, sealed)
}

fn worker_entry(shared: Arc<Shared>) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker_loop(&shared))).is_err() {
        fail_worker(&shared);
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let mut state = shared.lock();
        let (count, bytes) = loop {
            if state.queue.is_empty() {
                if !state.accepting {
                    state.exited = true;
                    shared.changed.notify_all();
                    return;
                }
                state = shared.wait(state);
                continue;
            }
            let (count, bytes, sealed) = batch_shape(&state);
            let remaining = BATCH_DELAY.saturating_sub(state.queue[0].accepted_at.elapsed());
            if sealed || remaining.is_zero() {
                break (count, bytes);
            }
            state = shared.wait_for(state, remaining);
        };
        let batch = state.queue.drain(..count).collect::<Vec<_>>();
        state.inflight_bytes = bytes;
        state.inflight_jobs = count;
        drop(state);

        let sequence = batch.last().unwrap().sequence;
        let chunks = batch
            .iter()
            .map(|job| job.bytes.as_slice())
            .collect::<Vec<_>>();
        let result = batch[0].target.write(&chunks, batch[0].offset);
        let error = match result {
            Ok(written) if written == bytes => None,
            Ok(_) => Some(io::Error::from_raw_os_error(libc::EIO)),
            Err(error) => Some(error),
        };
        // Keep all handle/activity leases until native I/O has returned. Drop
        // targets and data before returning the corresponding budget to callers.
        drop(chunks);
        drop(batch);
        let mut state = shared.lock();
        if let Some(error) = error {
            if state.failure.is_none() {
                state.failure = Some(Failure::capture(error));
            }
        }
        state.completed = sequence;
        state.bytes -= bytes;
        state.jobs -= count;
        state.inflight_bytes = 0;
        state.inflight_jobs = 0;
        shared.changed.notify_all();
    }
}

fn fail_worker(shared: &Shared) {
    let abandoned = {
        let mut state = shared.lock();
        if state.failure.is_none() {
            state.failure = Some(Failure::capture(io::Error::from_raw_os_error(libc::EIO)));
        }
        state.accepting = false;
        state.exited = true;
        let queue = std::mem::take(&mut state.queue);
        let bytes = queue.iter().map(|job| job.bytes.len()).sum::<usize>();
        let inflight_bytes = state.inflight_bytes;
        let inflight_jobs = state.inflight_jobs;
        state.bytes -= bytes + inflight_bytes;
        state.jobs -= queue.len() + inflight_jobs;
        state.inflight_bytes = 0;
        state.inflight_jobs = 0;
        shared.changed.notify_all();
        queue
    };
    // Releasing target leases may wake other I/O owners; hold no queue mutex.
    drop(abandoned);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};

    #[derive(Debug, PartialEq, Eq)]
    struct Call {
        key: (u64, bool),
        offset: u64,
        chunks: usize,
        bytes: Vec<u8>,
    }

    struct Recording {
        key: (u64, bool),
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl Target for Recording {
        fn merge_key(&self) -> (u64, bool) {
            self.key
        }

        fn write(&self, chunks: &[&[u8]], offset: u64) -> io::Result<usize> {
            let bytes = chunks.concat();
            let length = bytes.len();
            self.calls.lock().unwrap().push(Call {
                key: self.key,
                offset,
                chunks: chunks.len(),
                bytes,
            });
            Ok(length)
        }
    }

    struct Blocking {
        key: u64,
        entered: Sender<()>,
        release: Receiver<()>,
        live: Option<Arc<AtomicUsize>>,
    }

    impl Target for Blocking {
        fn merge_key(&self) -> (u64, bool) {
            (self.key, false)
        }

        fn write(&self, chunks: &[&[u8]], _: u64) -> io::Result<usize> {
            if let Some(live) = &self.live {
                assert_eq!(live.load(Ordering::SeqCst), 2);
            }
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(chunks.iter().map(|chunk| chunk.len()).sum())
        }
    }

    fn submit(buffer: &WriteBuffer, target: Box<dyn Target>, bytes: &[u8], offset: u64) -> u64 {
        buffer
            .reserve(bytes.len())
            .unwrap()
            .submit(target, bytes.to_vec(), offset)
            .unwrap()
    }

    fn recording(key: (u64, bool), calls: &Arc<Mutex<Vec<Call>>>) -> Box<dyn Target> {
        Box::new(Recording {
            key,
            calls: calls.clone(),
        })
    }

    #[test]
    fn adjacent_writes_merge_but_offsets_handles_and_privilege_flags_do_not() {
        let buffer = WriteBuffer::new().unwrap();
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 100,
                entered,
                release: finish,
                live: None,
            }),
            b"block",
            0,
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        submit(&buffer, recording((1, false), &calls), b"abc", 0);
        submit(&buffer, recording((1, false), &calls), b"def", 3);
        submit(&buffer, recording((1, true), &calls), b"g", 6);
        submit(&buffer, recording((2, true), &calls), b"h", 7);
        submit(&buffer, recording((2, true), &calls), b"i", 11);
        release.send(()).unwrap();
        buffer.flush().unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call {
                    key: (1, false),
                    offset: 0,
                    chunks: 2,
                    bytes: b"abcdef".to_vec()
                },
                Call {
                    key: (1, true),
                    offset: 6,
                    chunks: 1,
                    bytes: b"g".to_vec()
                },
                Call {
                    key: (2, true),
                    offset: 7,
                    chunks: 1,
                    bytes: b"h".to_vec()
                },
                Call {
                    key: (2, true),
                    offset: 11,
                    chunks: 1,
                    bytes: b"i".to_vec()
                },
            ]
        );
        buffer.stop().unwrap();
    }

    #[test]
    fn reserved_queued_and_inflight_bytes_all_hold_the_budget() {
        let buffer = Arc::new(WriteBuffer::with_limits(4, 1).unwrap());
        let permit = buffer.reserve(4).unwrap();
        let (ready, result) = mpsc::channel();
        let waiter = thread::spawn({
            let buffer = buffer.clone();
            move || ready.send(buffer.reserve(1)).unwrap()
        });
        assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        permit
            .submit(
                Box::new(Blocking {
                    key: 1,
                    entered,
                    release: finish,
                    live: None,
                }),
                b"data".to_vec(),
                0,
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        {
            let state = buffer.shared.lock();
            assert_eq!((state.bytes, state.jobs), (4, 1));
        }
        assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        drop(
            result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
        );
        waiter.join().unwrap();
        buffer.flush().unwrap();
        buffer.stop().unwrap();
        assert_eq!(buffer.shared.lock().bytes, 0);
    }

    struct Short;
    impl Target for Short {
        fn merge_key(&self) -> (u64, bool) {
            (1, false)
        }
        fn write(&self, _: &[&[u8]], _: u64) -> io::Result<usize> {
            Ok(1)
        }
    }

    #[test]
    fn partial_write_is_sticky_eio_and_reservations_refund_after_rejection() {
        let buffer = WriteBuffer::with_limits(8, 2).unwrap();
        let late = buffer.reserve(1).unwrap();
        submit(&buffer, Box::new(Short), b"abcd", 0);
        assert_eq!(buffer.flush().unwrap_err().raw_os_error(), Some(libc::EIO));
        assert_eq!(
            buffer.check_error().unwrap_err().raw_os_error(),
            Some(libc::EIO)
        );
        assert!(buffer.reserve(1).is_err());
        assert_eq!(
            late.submit(Box::new(Short), vec![0], 0)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(buffer.shared.lock().bytes, 0);
        assert_eq!(buffer.stop().unwrap_err().raw_os_error(), Some(libc::EIO));
        assert_eq!(buffer.flush().unwrap_err().raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn flush_waits_its_accepted_cut_without_waiting_for_later_work() {
        let buffer = Arc::new(WriteBuffer::new().unwrap());
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 1,
                entered,
                release: finish,
                live: None,
            }),
            b"a",
            0,
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (flushed, done) = mpsc::channel();
        let flusher = thread::spawn({
            let buffer = buffer.clone();
            move || flushed.send(buffer.flush()).unwrap()
        });
        {
            let mut state = buffer.shared.lock();
            while !state.cuts.contains_key(&1) {
                state = buffer.shared.wait(state);
            }
        }
        let (entered2, started2) = mpsc::channel();
        let (release2, finish2) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 2,
                entered: entered2,
                release: finish2,
                live: None,
            }),
            b"b",
            0,
        );
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        started2.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(buffer.shared.lock().completed, 1);
        release2.send(()).unwrap();
        flusher.join().unwrap();
        buffer.stop().unwrap();
    }

    struct Panics;
    impl Target for Panics {
        fn merge_key(&self) -> (u64, bool) {
            (1, false)
        }
        fn write(&self, _: &[&[u8]], _: u64) -> io::Result<usize> {
            panic!("test native writer panic")
        }
    }

    #[test]
    fn worker_panic_wakes_barrier_and_budget_waiters_with_sticky_eio() {
        let buffer = Arc::new(WriteBuffer::with_limits(1, 1).unwrap());
        submit(&buffer, Box::new(Panics), b"a", 0);
        let (tx, rx) = mpsc::channel();
        let waiter = thread::spawn({
            let buffer = buffer.clone();
            move || tx.send(buffer.reserve(1).map(|_| ())).unwrap()
        });
        assert_eq!(buffer.flush().unwrap_err().raw_os_error(), Some(libc::EIO));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
        waiter.join().unwrap();
        assert_eq!(buffer.stop().unwrap_err().raw_os_error(), Some(libc::EIO));
        {
            let state = buffer.shared.lock();
            assert_eq!((state.bytes, state.jobs), (0, 0));
        }
    }

    #[test]
    fn stop_drains_accepted_writes_but_invalidates_unsubmitted_permits() {
        let buffer = WriteBuffer::with_limits(8, 2).unwrap();
        let late = buffer.reserve(4).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        submit(&buffer, recording((1, false), &calls), b"data", 0);
        buffer.stop().unwrap();
        assert_eq!(calls.lock().unwrap()[0].bytes, b"data");
        assert_eq!(
            late.submit(recording((1, false), &calls), vec![0; 4], 4)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(buffer.reserve(1).is_err());
        buffer.flush().unwrap();
        buffer.stop().unwrap();
        assert_eq!(buffer.shared.lock().bytes, 0);
    }

    #[test]
    fn allocation_and_reservation_rejection_refund_bytes_and_job_slots() {
        let buffer = WriteBuffer::with_limits(4, 2).unwrap();
        assert!(buffer.reserve(5).is_err());
        let permit = buffer.reserve(1).unwrap();
        let mut oversized = Vec::with_capacity(4);
        oversized.push(1);
        assert!(permit.submit(Box::new(Short), oversized, 0).is_err());
        let permit = buffer.reserve(4).unwrap();
        assert!(permit
            .submit(Box::new(Short), vec![0; 4], u64::MAX)
            .is_err());
        let permit = buffer.reserve(4).unwrap();
        drop(permit);
        let state = buffer.shared.lock();
        assert_eq!((state.bytes, state.jobs), (0, 0));
    }

    #[test]
    fn coalescing_obeys_chunk_and_byte_limits() {
        let buffer = WriteBuffer::new().unwrap();
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 100,
                entered,
                release: finish,
                live: None,
            }),
            b"block",
            0,
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        for offset in 0..65 {
            submit(&buffer, recording((1, false), &calls), b"x", offset);
        }
        let half = vec![0; BATCH_BYTES / 2];
        submit(&buffer, recording((2, false), &calls), &half, 0);
        submit(
            &buffer,
            recording((2, false), &calls),
            &half,
            half.len() as u64,
        );
        submit(
            &buffer,
            recording((2, false), &calls),
            b"x",
            BATCH_BYTES as u64,
        );
        release.send(()).unwrap();
        buffer.flush().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!((calls[0].chunks, calls[0].bytes.len()), (64, 64));
        assert_eq!((calls[1].chunks, calls[1].bytes.len()), (1, 1));
        assert_eq!((calls[2].chunks, calls[2].bytes.len()), (2, BATCH_BYTES));
        assert_eq!((calls[3].chunks, calls[3].bytes.len()), (1, 1));
    }

    struct LeaseTarget {
        recording: Recording,
        live: Arc<AtomicUsize>,
    }

    impl Target for LeaseTarget {
        fn merge_key(&self) -> (u64, bool) {
            self.recording.key
        }
        fn write(&self, chunks: &[&[u8]], offset: u64) -> io::Result<usize> {
            assert_eq!(self.live.load(Ordering::SeqCst), 2);
            self.recording.write(chunks, offset)
        }
    }

    impl Drop for LeaseTarget {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn merged_targets_live_until_io_returns_and_drop_before_flush_completes() {
        let buffer = WriteBuffer::new().unwrap();
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 100,
                entered,
                release: finish,
                live: None,
            }),
            b"block",
            0,
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let live = Arc::new(AtomicUsize::new(2));
        for offset in 0..2 {
            submit(
                &buffer,
                Box::new(LeaseTarget {
                    recording: Recording {
                        key: (1, false),
                        calls: calls.clone(),
                    },
                    live: live.clone(),
                }),
                b"x",
                offset,
            );
        }
        release.send(()).unwrap();
        buffer.flush().unwrap();
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(calls.lock().unwrap()[0].chunks, 2);
    }

    #[test]
    fn sticky_error_does_not_let_flush_or_stop_skip_already_accepted_work() {
        let buffer = Arc::new(WriteBuffer::new().unwrap());
        let (entered, started) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 100,
                entered,
                release: finish,
                live: None,
            }),
            b"block",
            0,
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        submit(&buffer, Box::new(Short), b"abcd", 0);
        let (entered2, started2) = mpsc::channel();
        let (release2, finish2) = mpsc::channel();
        submit(
            &buffer,
            Box::new(Blocking {
                key: 2,
                entered: entered2,
                release: finish2,
                live: None,
            }),
            b"last",
            0,
        );
        release.send(()).unwrap();
        started2.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            buffer.check_error().unwrap_err().raw_os_error(),
            Some(libc::EIO)
        );
        let (flushed, flush_result) = mpsc::channel();
        let flusher = thread::spawn({
            let buffer = buffer.clone();
            move || flushed.send(buffer.flush()).unwrap()
        });
        let (stopped, stop_result) = mpsc::channel();
        let stopper = thread::spawn({
            let buffer = buffer.clone();
            move || stopped.send(buffer.stop()).unwrap()
        });
        assert!(flush_result
            .recv_timeout(Duration::from_millis(20))
            .is_err());
        assert!(stop_result.recv_timeout(Duration::from_millis(20)).is_err());
        release2.send(()).unwrap();
        assert_eq!(
            flush_result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(
            stop_result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
        flusher.join().unwrap();
        stopper.join().unwrap();
    }
}

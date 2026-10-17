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

/// Private, session-local bounds for host-owned staging and write coalescing.
/// A single job is never split to meet `batch_bytes`: a larger accepted job is
/// executed alone. `max_chunks = 1` and zero delay disable coalescing.
#[derive(Clone, Copy, Debug)]
pub(super) struct WriteBufferOptions {
    pub(super) budget_bytes: usize,
    pub(super) max_jobs: usize,
    pub(super) batch_bytes: usize,
    pub(super) max_chunks: usize,
    pub(super) max_delay: Duration,
}

impl Default for WriteBufferOptions {
    fn default() -> Self {
        Self {
            budget_bytes: 64 * 1024 * 1024,
            max_jobs: 1024,
            batch_bytes: 1024 * 1024,
            max_chunks: 64,
            max_delay: Duration::from_millis(10),
        }
    }
}

impl WriteBufferOptions {
    /// Rejects unsupported bounds before a worker or queue is created.
    pub(super) fn validate(&self) -> io::Result<()> {
        let invalid = if !(4 * 1024 * 1024..=256 * 1024 * 1024).contains(&self.budget_bytes) {
            Some("write-buffer budget_bytes must be between 4 MiB and 256 MiB")
        } else if !(1..=4096).contains(&self.max_jobs) {
            Some("write-buffer max_jobs must be between 1 and 4096")
        } else if !(4 * 1024..=4 * 1024 * 1024).contains(&self.batch_bytes) {
            Some("write-buffer batch_bytes must be between 4 KiB and 4 MiB")
        } else if self.batch_bytes > self.budget_bytes {
            Some("write-buffer batch_bytes must not exceed budget_bytes")
        } else if !(1..=64).contains(&self.max_chunks) {
            Some("write-buffer max_chunks must be between 1 and 64")
        } else if self.max_delay > Duration::from_millis(50) {
            Some("write-buffer max_delay must be between zero and 50 ms")
        } else {
            None
        };
        match invalid {
            Some(message) => Err(io::Error::new(io::ErrorKind::InvalidInput, message)),
            None => Ok(()),
        }
    }
}

/// Cumulative session diagnostics. Peaks include reserved, queued and executing
/// work; jobs count accepted submissions, and batches count native write calls
/// started, including calls that fail or unwind. Counters saturate, never reset.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WriteBufferStats {
    pub(super) peak_bytes: usize,
    pub(super) peak_jobs: usize,
    pub(super) batches: u64,
    pub(super) total_jobs: u64,
    /// Number of reserve calls that waited for capacity, including failed calls.
    pub(super) reserve_wait_count: u64,
    /// Time inside capacity waits, including mutex reacquisition after wakeup;
    /// excludes initial mutex acquisition and records each wait when it returns.
    pub(super) reserve_wait_ns: u64,
}

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
    stats: WriteBufferStats,
    // Controls do not consume the data budget. A cut prevents a later write
    // joining its batch, so continuous producers cannot starve a barrier.
    cuts: BTreeMap<u64, usize>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    options: WriteBufferOptions,
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
    profile: Option<pvisor_overlay_core::profile::Profile>,
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
    #[cfg(test)]
    pub(super) fn new() -> io::Result<Self> {
        Self::with_options(WriteBufferOptions::default())
    }

    pub(super) fn with_options(options: WriteBufferOptions) -> io::Result<Self> {
        options.validate()?;
        Self::start(options)
    }

    pub(super) fn with_profile(
        options: WriteBufferOptions,
        profile: pvisor_overlay_core::profile::Profile,
    ) -> io::Result<Self> {
        let mut buffer = Self::with_options(options)?;
        buffer.profile = Some(profile);
        Ok(buffer)
    }

    fn start(options: WriteBufferOptions) -> io::Result<Self> {
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
                stats: WriteBufferStats::default(),
                cuts: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            options,
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("pvisor-fs-write-buffer".into())
            .spawn(move || worker_entry(worker_shared))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
            profile: None,
        })
    }

    // Tiny budgets make accounting/failure tests independent of MiB allocations.
    // Production construction always validates the supported options above.
    #[cfg(test)]
    fn with_limits(max_bytes: usize, max_jobs: usize) -> io::Result<Self> {
        assert!(max_bytes != 0 && max_jobs != 0);
        Self::start(WriteBufferOptions {
            budget_bytes: max_bytes,
            max_jobs,
            batch_bytes: max_bytes,
            ..WriteBufferOptions::default()
        })
    }

    /// Waits for space without holding the queue mutex or a native write lock.
    /// The caller may still hold its file-I/O and current virtqueue RAM leases;
    /// backpressure does not promise that an outstanding guest request can drain
    /// for VM offload. Do not hold a lock needed by queued targets to progress.
    /// Requests larger than the entire budget fail instead of waiting forever.
    pub(super) fn reserve(&self, size: usize) -> io::Result<Permit> {
        if size > self.shared.options.budget_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write exceeds the complete write-buffer byte budget",
            ));
        }
        let mut state = self.shared.lock();
        let mut waited = false;
        loop {
            admit(&state)?;
            if state.jobs < self.shared.options.max_jobs
                && size <= self.shared.options.budget_bytes - state.bytes
            {
                state.bytes += size;
                state.jobs += 1;
                state.stats.peak_bytes = state.stats.peak_bytes.max(state.bytes);
                state.stats.peak_jobs = state.stats.peak_jobs.max(state.jobs);
                return Ok(Permit {
                    shared: self.shared.clone(),
                    size,
                    held: true,
                });
            }
            if !waited {
                state.stats.reserve_wait_count = state.stats.reserve_wait_count.saturating_add(1);
                waited = true;
            }
            let wait_started = Instant::now();
            state = self.shared.wait(state);
            let elapsed = u64::try_from(wait_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            state.stats.reserve_wait_ns = state.stats.reserve_wait_ns.saturating_add(elapsed);
        }
    }

    /// Returns one consistent, session-local cumulative snapshot. Add deltas for
    /// counters across snapshots and take maxima for peaks; do not sum snapshots.
    pub(super) fn stats(&self) -> WriteBufferStats {
        self.shared.lock().stats
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
        if let Some(profile) = &self.profile {
            let stats = self.stats();
            // Exactly once per session, after its final worker has exited.
            // Peak counters are sums across sessions, not repeated snapshots.
            for (name, value) in [
                ("host_writeback_queue_sessions", 1),
                (
                    "host_writeback_queue_peak_bytes_sum",
                    stats.peak_bytes as u64,
                ),
                ("host_writeback_queue_peak_jobs_sum", stats.peak_jobs as u64),
                ("host_writeback_queue_batches", stats.batches),
                ("host_writeback_queue_jobs", stats.total_jobs),
                (
                    "host_writeback_reserve_wait_count",
                    stats.reserve_wait_count,
                ),
                ("host_writeback_reserve_wait_ns", stats.reserve_wait_ns),
            ] {
                profile.add(name, value);
            }
        }
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
        state.stats.total_jobs = sequence;
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

fn batch_shape(state: &State, options: &WriteBufferOptions) -> (usize, usize, bool) {
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
                || bytes + job.bytes.len() > options.batch_bytes))
            || count == options.max_chunks
            || cut.is_some_and(|cut| job.sequence > cut)
        {
            sealed = true;
            break;
        }
        count += 1;
        bytes += job.bytes.len();
        next_offset += job.bytes.len() as u64;
        if bytes >= options.batch_bytes || count == options.max_chunks {
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
            let (count, bytes, sealed) = batch_shape(&state, &shared.options);
            let remaining = shared
                .options
                .max_delay
                .saturating_sub(state.queue[0].accepted_at.elapsed());
            if sealed || remaining.is_zero() {
                break (count, bytes);
            }
            state = shared.wait_for(state, remaining);
        };
        let batch = state.queue.drain(..count).collect::<Vec<_>>();
        state.inflight_bytes = bytes;
        state.inflight_jobs = count;
        state.stats.batches = state.stats.batches.saturating_add(1);
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
    fn options_preserve_defaults_and_accept_supported_boundaries() {
        let defaults = WriteBufferOptions::default();
        assert_eq!(defaults.budget_bytes, 64 * 1024 * 1024);
        assert_eq!(defaults.max_jobs, 1024);
        assert_eq!(defaults.batch_bytes, 1024 * 1024);
        assert_eq!(defaults.max_chunks, 64);
        assert_eq!(defaults.max_delay, Duration::from_millis(10));
        defaults.validate().unwrap();
        let minimum = WriteBufferOptions {
            budget_bytes: 4 * 1024 * 1024,
            max_jobs: 1,
            batch_bytes: 4 * 1024,
            max_chunks: 1,
            max_delay: Duration::ZERO,
        };
        let maximum = WriteBufferOptions {
            budget_bytes: 256 * 1024 * 1024,
            max_jobs: 4096,
            batch_bytes: 4 * 1024 * 1024,
            max_chunks: 64,
            max_delay: Duration::from_millis(50),
        };
        for options in [minimum, maximum] {
            options.validate().unwrap();
            let buffer = WriteBuffer::with_options(options).unwrap();
            assert_eq!(buffer.shared.options.budget_bytes, options.budget_bytes);
            assert_eq!(buffer.shared.options.max_jobs, options.max_jobs);
            assert_eq!(buffer.shared.options.batch_bytes, options.batch_bytes);
            assert_eq!(buffer.shared.options.max_chunks, options.max_chunks);
            assert_eq!(buffer.shared.options.max_delay, options.max_delay);
            buffer.stop().unwrap();
        }
    }

    #[test]
    fn options_reject_each_out_of_range_value_before_starting_a_worker() {
        let defaults = WriteBufferOptions::default();
        let invalid = [
            WriteBufferOptions {
                budget_bytes: 4 * 1024 * 1024 - 1,
                ..defaults
            },
            WriteBufferOptions {
                budget_bytes: 256 * 1024 * 1024 + 1,
                ..defaults
            },
            WriteBufferOptions {
                max_jobs: 0,
                ..defaults
            },
            WriteBufferOptions {
                max_jobs: 4097,
                ..defaults
            },
            WriteBufferOptions {
                batch_bytes: 4 * 1024 - 1,
                ..defaults
            },
            WriteBufferOptions {
                batch_bytes: 4 * 1024 * 1024 + 1,
                ..defaults
            },
            WriteBufferOptions {
                max_chunks: 0,
                ..defaults
            },
            WriteBufferOptions {
                max_chunks: 65,
                ..defaults
            },
            WriteBufferOptions {
                max_delay: Duration::from_millis(50) + Duration::from_nanos(1),
                ..defaults
            },
        ];
        for options in invalid {
            assert_eq!(
                options.validate().unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{options:?}"
            );
            assert_eq!(
                WriteBuffer::with_options(options).err().unwrap().kind(),
                io::ErrorKind::InvalidInput,
                "{options:?}"
            );
        }
    }

    #[test]
    fn custom_options_bound_merging_and_report_session_stats() {
        for (batch_bytes, max_chunks, expected_chunks) in [
            (4096, 64, vec![2, 2, 1]),
            (8192, 2, vec![2, 2, 1]),
            (4096, 1, vec![1, 1, 1, 1, 1]),
        ] {
            let buffer = WriteBuffer::with_options(WriteBufferOptions {
                budget_bytes: 4 * 1024 * 1024,
                max_jobs: 6,
                batch_bytes,
                max_chunks,
                max_delay: Duration::ZERO,
            })
            .unwrap();
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
            // Zero-delay work progresses without a flush or another producer.
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let bytes = vec![1; 2048];
            for offset in 0..5 {
                submit(
                    &buffer,
                    recording((1, false), &calls),
                    &bytes,
                    offset * bytes.len() as u64,
                );
            }
            let pending = buffer.stats();
            assert_eq!(pending.peak_bytes, 5 + 5 * bytes.len());
            assert_eq!(pending.peak_jobs, 6);
            assert_eq!((pending.batches, pending.total_jobs), (1, 6));
            release.send(()).unwrap();
            buffer.flush().unwrap();
            let calls = calls.lock().unwrap();
            assert_eq!(
                calls.iter().map(|call| call.chunks).collect::<Vec<_>>(),
                expected_chunks
            );
            assert_eq!(calls.iter().flat_map(|call| &call.bytes).count(), 10240);
            let stats = buffer.stats();
            assert_eq!(stats.peak_bytes, pending.peak_bytes);
            assert_eq!(stats.peak_jobs, pending.peak_jobs);
            assert_eq!(stats.total_jobs, 6);
            assert_eq!(stats.batches, 1 + calls.len() as u64);
            assert_eq!(stats.reserve_wait_count, 0);
            assert_eq!(stats.reserve_wait_ns, 0);
            drop(calls);
            buffer.stop().unwrap();
        }
    }

    #[test]
    fn profiled_session_totals_publish_once_only_after_last_owner_drop() {
        let profile = pvisor_overlay_core::profile::Profile::enabled("write-buffer-owner-test");
        let buffer = Arc::new(
            WriteBuffer::with_profile(WriteBufferOptions::default(), profile.clone()).unwrap(),
        );
        // The unused reservation contributes to the true peak, but is not an
        // accepted job. It may safely outlive the stopped buffer owner.
        let reserved = buffer.reserve(3).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        submit(&buffer, recording((1, false), &calls), b"data", 0);
        buffer.flush().unwrap();
        buffer.stop().unwrap();
        buffer.stop().unwrap();
        let stats = buffer.stats();
        assert_eq!((stats.peak_bytes, stats.peak_jobs), (7, 2));
        assert_eq!((stats.total_jobs, stats.batches), (1, 1));
        let units = |name| {
            profile
                .report()
                .unwrap()
                .measurements
                .get(name)
                .map_or(0, |measurement| measurement.units)
        };
        assert_eq!(units("host_writeback_queue_sessions"), 0);
        let retained = buffer.clone();
        drop(buffer);
        assert_eq!(units("host_writeback_queue_sessions"), 0);
        drop(retained);
        for (name, expected) in [
            ("host_writeback_queue_sessions", 1),
            ("host_writeback_queue_peak_bytes_sum", 7),
            ("host_writeback_queue_peak_jobs_sum", 2),
            ("host_writeback_queue_jobs", 1),
            ("host_writeback_queue_batches", 1),
            ("host_writeback_reserve_wait_count", 0),
            ("host_writeback_reserve_wait_ns", 0),
        ] {
            assert_eq!(units(name), expected, "{name}");
        }
        drop(reserved);
        assert_eq!(units("host_writeback_queue_sessions"), 1);
        assert_eq!(units("host_writeback_queue_peak_bytes_sum"), 7);
        assert_eq!(calls.lock().unwrap()[0].bytes, b"data");
    }

    #[test]
    fn one_write_larger_than_batch_target_is_executed_alone_without_splitting() {
        let buffer = WriteBuffer::with_options(WriteBufferOptions {
            batch_bytes: 4096,
            max_delay: Duration::ZERO,
            ..WriteBufferOptions::default()
        })
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let bytes = vec![1; 8192];
        submit(&buffer, recording((1, false), &calls), &bytes, 0);
        submit(&buffer, recording((1, false), &calls), b"x", 8192);
        buffer.flush().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].chunks, calls[0].bytes.len()), (1, 8192));
        assert_eq!((calls[1].chunks, calls[1].bytes.len()), (1, 1));
    }

    #[test]
    fn owned_input_completes_with_guest_ram_gate_closed() {
        use crate::devices::virtio::descriptor_utils::{
            create_descriptor_chain, DescriptorType, OwnedDescriptorChain,
        };
        use crate::devices::virtio::memory_gate;
        use std::io::Read;
        use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

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

        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let gate = memory_gate::register(&mem);
        let input = GuestAddress(0x1000);
        let bytes = b"accepted";
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            input,
            vec![
                (DescriptorType::Readable, bytes.len() as u32),
                (DescriptorType::Writable, 16),
            ],
            0,
        )
        .unwrap();
        mem.write_slice(bytes, input).unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        let (mut reader, writer) = plan.reader_writer(&mem).unwrap();
        let permit = buffer.reserve(bytes.len()).unwrap();
        let mut owned = vec![0; bytes.len()];
        reader.read_exact(&mut owned).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        permit
            .submit(recording((1, false), &calls), owned, 0)
            .unwrap();
        drop((reader, writer));
        assert!(!gate.try_close().unwrap());

        // The descriptor owner releases RAM only after publishing its reply.
        drop(plan);
        mem.write_slice(b"reused!!", input).unwrap();
        assert!(gate.try_close().unwrap());
        release.send(()).unwrap();
        buffer.flush().unwrap();
        assert_eq!(calls.lock().unwrap()[0].bytes, bytes);
        assert!(gate.is_idle_closed());
        let state = buffer.shared.lock();
        assert_eq!((state.bytes, state.jobs), (0, 0));
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
        let stats = buffer.stats();
        assert_eq!((stats.peak_bytes, stats.peak_jobs), (4, 1));
        assert_eq!((stats.total_jobs, stats.batches), (1, 1));
        assert_eq!(stats.reserve_wait_count, 1);
        assert!(stats.reserve_wait_ns > 0);
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
        let batch_bytes = WriteBufferOptions::default().batch_bytes;
        let half = vec![0; batch_bytes / 2];
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
            batch_bytes as u64,
        );
        release.send(()).unwrap();
        buffer.flush().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!((calls[0].chunks, calls[0].bytes.len()), (64, 64));
        assert_eq!((calls[1].chunks, calls[1].bytes.len()), (1, 1));
        assert_eq!((calls[2].chunks, calls[2].bytes.len()), (2, batch_bytes));
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

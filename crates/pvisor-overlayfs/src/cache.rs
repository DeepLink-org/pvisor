//! Linux owned-view notification transport. Never wait in a FUSE callback.
//!
//! Attribute-only inode notifications cannot trigger writeback. A separate
//! entry worker may wait on kernel namespace locks; it must never be responsible
//! for sending the replies that release those locks. See libfuse's lowlevel
//! notify_inval_entry/notify_inval_inode deadlock contracts.
use fuser::Notifier;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;

const QUEUE_LIMIT: usize = 256;
const PENDING_LIMIT: usize = 512;
pub(crate) const EFFECT_LIMIT: usize = 4096;
const TERMINATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default, Debug)]
pub(crate) struct Effects {
    pub(crate) inodes: BTreeSet<u64>,
    pub(crate) entries: BTreeSet<(u64, OsString)>,
    pub(crate) overflow: bool,
}

trait NotificationBackend: Send + Sync {
    fn attributes(&self, ino: u64) -> io::Result<()>;
    fn entry(&self, parent: u64, name: &std::ffi::OsStr) -> io::Result<()>;
}

impl NotificationBackend for Notifier {
    fn attributes(&self, ino: u64) -> io::Result<()> {
        self.inval_inode(ino, -1, 0)
    }
    fn entry(&self, parent: u64, name: &std::ffi::OsStr) -> io::Result<()> {
        self.inval_entry(parent, name)
    }
}

struct State {
    pending: AtomicUsize,
    failed: AtomicBool,
    stopping: AtomicBool,
    shutdown: AtomicBool,
    error: Mutex<Option<String>>,
    terminated: Arc<(Mutex<bool>, Condvar)>,
    abort: Box<dyn Fn() -> io::Result<()> + Send + Sync>,
}

impl State {
    fn notification_error(&self, error: io::Error) {
        if self.shutdown.load(Ordering::SeqCst) && error.raw_os_error() == Some(libc::ENODEV) {
            return; // Normal notification race with the completed mount detachment.
        }
        self.fail(error);
    }

    fn await_termination(&self) {
        if !self.failed.load(Ordering::SeqCst) {
            return;
        }
        let mut done = self.terminated.0.lock().unwrap();
        let deadline = std::time::Instant::now() + TERMINATION_TIMEOUT;
        while !*done {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                std::process::abort();
            }
            done = self.terminated.1.wait_timeout(done, remaining).unwrap().0;
        }
    }

    fn fail(&self, error: impl std::fmt::Display) {
        self.stopping.store(true, Ordering::SeqCst);
        if self.failed.swap(true, Ordering::SeqCst) {
            self.await_termination();
            return;
        }
        let message = format!("kernel cache invalidation failed: {error}");
        *self.error.lock().unwrap() = Some(message.clone());
        let completion = self.terminated.clone();
        let deadline = std::thread::Builder::new()
            .name("overlay-cache-deadline".into())
            .spawn(move || {
                let done = completion.0.lock().unwrap();
                let (done, _) = completion
                    .1
                    .wait_timeout_while(done, TERMINATION_TIMEOUT, |done| !*done)
                    .unwrap();
                if !*done {
                    std::process::abort();
                }
            })
            .unwrap_or_else(|_| std::process::abort());
        if let Err(error) = (self.abort)() {
            // Neither this session nor its hosting owner may continue after
            // losing the ability to abort/detach. Treat that as a fatal OS /
            // lifecycle contract failure, not as a degraded cache mode.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "cannot terminate failed FUSE connection: {error}; terminating process"
            );
            std::process::abort();
        }
        *self.terminated.0.lock().unwrap() = true;
        self.terminated.1.notify_all();
        let _ = deadline.join();
        // An application logger must not be able to prevent failure detachment.
        log::error!("{message}; FUSE mount detached and connection aborted");
    }
}

type Reply = Box<dyn FnOnce(bool) + Send>;
struct Task {
    effects: Effects,
    reply: Reply,
}

#[derive(Clone)]
pub(crate) struct CacheHandle {
    replies: mpsc::SyncSender<Task>,
    failures: mpsc::SyncSender<Option<Task>>,
    state: Arc<State>,
}

impl CacheHandle {
    pub(crate) fn stopped(&self) -> bool {
        self.state.stopping.load(Ordering::SeqCst)
    }

    pub(crate) fn stop(&self) {
        if !self.state.stopping.swap(true, Ordering::SeqCst) {
            // One wakeup, no work or wait in the callback. Reserved tasks plus
            // this single control message fit the globally bounded stop queue.
            let _ = self.failures.try_send(None);
        }
    }

    // Keep the atomic update spelling supported by Rust versions before 1.99.
    #[allow(deprecated)]
    pub(crate) fn begin(&self) -> bool {
        if self.stopped() {
            return false;
        }
        if self
            .state
            .pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| {
                (pending < PENDING_LIMIT).then_some(pending + 1)
            })
            .is_err()
        {
            self.stop();
            return false;
        }
        true
    }

    pub(crate) fn pending(&self) -> bool {
        self.state.pending.load(Ordering::SeqCst) != 0
            || self.stopped()
            || self.state.failed.load(Ordering::SeqCst)
    }

    pub(crate) fn submit(&self, effects: Effects, reply: impl FnOnce(bool) + Send + 'static) {
        let overflow = effects.overflow;
        let task = Task {
            effects,
            reply: Box::new(reply),
        };
        if overflow {
            self.stop();
        }
        let result = if overflow {
            Err(mpsc::TrySendError::Full(task))
        } else {
            self.replies.try_send(task)
        };
        if let Err(error) = result {
            let task = match error {
                mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
            };
            // Even queue failure must not run detach/wait in a FUSE callback.
            // The stop-only control queue owns the failed reply until teardown.
            self.stop();
            if self.failures.try_send(Some(task)).is_err() {
                // At most PENDING_LIMIT tasks can exist. A full/disconnected
                // control queue here violates the transport's ownership bound.
                std::process::abort();
            }
        }
    }
}

pub(crate) struct CacheWorkers {
    replies: JoinHandle<()>,
    entries: JoinHandle<()>,
    failures: JoinHandle<()>,
    state: Arc<State>,
}

impl std::fmt::Debug for CacheWorkers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheWorkers")
            .field("pending", &self.state.pending.load(Ordering::SeqCst))
            .field("failed", &self.state.failed.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl CacheWorkers {
    pub(crate) fn start(
        notifier: Notifier,
        abort: std::fs::File,
        mountpoint: &std::path::Path,
    ) -> io::Result<(CacheHandle, Self)> {
        let mountpoint = mountpoint.to_path_buf();
        Self::start_backend(
            Arc::new(notifier),
            Box::new(move || abort_and_detach(&abort, &mountpoint)),
        )
    }

    fn start_backend(
        backend: Arc<dyn NotificationBackend>,
        abort: Box<dyn Fn() -> io::Result<()> + Send + Sync>,
    ) -> io::Result<(CacheHandle, Self)> {
        let state = Arc::new(State {
            pending: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            error: Mutex::new(None),
            terminated: Arc::new((Mutex::new(false), Condvar::new())),
            abort,
        });
        let (reply_tx, reply_rx) = mpsc::sync_channel::<Task>(QUEUE_LIMIT);
        let (entry_tx, entry_rx) = mpsc::sync_channel::<Effects>(QUEUE_LIMIT);
        let entry_state = state.clone();
        let entry_backend = backend.clone();
        let entries = std::thread::Builder::new()
            .name("overlay-cache-entries".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for effects in entry_rx {
                        if !entry_state.failed.load(Ordering::SeqCst) {
                            for (parent, name) in &effects.entries {
                                if let Err(error) = entry_backend.entry(*parent, name) {
                                    entry_state.notification_error(error);
                                    break;
                                }
                            }
                            // Reads during the post-reply window used zero TTL.
                            // Expire any attributes the kernel updated in reply handling.
                            for ino in &effects.inodes {
                                if let Err(error) = entry_backend.attributes(*ino) {
                                    entry_state.notification_error(error);
                                    break;
                                }
                            }
                        }
                        entry_state.pending.fetch_sub(1, Ordering::SeqCst);
                    }
                }));
                if result.is_err() {
                    entry_state.fail("entry worker panicked");
                }
            })?;
        let (failure_tx, failure_rx) = mpsc::sync_channel::<Option<Task>>(PENDING_LIMIT + 1);
        let failure_state = state.clone();
        let failures = std::thread::Builder::new()
            .name("overlay-cache-stop".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for task in failure_rx {
                        failure_state.fail("mutation admission/effects/notification queue bound exceeded or disconnected");
                        if let Some(task) = task {
                            (task.reply)(false);
                            failure_state.pending.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }));
                if result.is_err() {
                    failure_state.fail("failure-control worker panicked");
                }
            })?;
        let reply_state = state.clone();
        let replies = std::thread::Builder::new()
            .name("overlay-cache-replies".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for task in reply_rx {
                        if !reply_state.failed.load(Ordering::SeqCst) {
                            for ino in &task.effects.inodes {
                                if let Err(error) = backend.attributes(*ino) {
                                    reply_state.notification_error(error);
                                    break;
                                }
                            }
                        }
                        // Capture the reply's validity once: a successful reply may
                        // linearize before a concurrent failure, but a failed reply
                        // must never wake its syscall before detachment completes.
                        let valid = !reply_state.failed.load(Ordering::SeqCst);
                        if !valid {
                            reply_state.await_termination();
                        }
                        // Attribute expiry precedes completion of the mutation syscall.
                        // The kernel handles its cooperative dentry/page-cache effects.
                        (task.reply)(valid);
                        // This worker must never block on the entry worker: another
                        // related syscall could hold a namespace lock waiting for us.
                        if entry_tx.try_send(task.effects).is_err() {
                            reply_state.fail("entry notification queue disconnected or full");
                            reply_state.pending.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }));
                if result.is_err() {
                    reply_state.fail("reply worker panicked");
                }
            })?;
        Ok((
            CacheHandle {
                replies: reply_tx,
                failures: failure_tx,
                state: state.clone(),
            },
            Self {
                replies,
                entries,
                failures,
                state,
            },
        ))
    }

    pub(crate) fn shutdown(&self) {
        self.state.shutdown.store(true, Ordering::SeqCst);
    }

    pub(crate) fn join(self) -> io::Result<()> {
        let deadline = std::time::Instant::now() + TERMINATION_TIMEOUT;
        while !self.replies.is_finished()
            || !self.entries.is_finished()
            || !self.failures.is_finished()
        {
            if std::time::Instant::now() >= deadline {
                self.state
                    .fail("notification worker shutdown deadline exceeded");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let deadline = std::time::Instant::now() + TERMINATION_TIMEOUT;
        while !self.replies.is_finished()
            || !self.entries.is_finished()
            || !self.failures.is_finished()
        {
            if std::time::Instant::now() >= deadline {
                std::process::abort();
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = self.replies.join();
        let _ = self.entries.join();
        let _ = self.failures.join();
        let error = self.state.error.lock().unwrap().clone();
        match error {
            Some(error) => Err(io::Error::other(error)),
            None => Ok(()),
        }
    }
}

pub(crate) fn abort_and_detach(
    abort: &std::fs::File,
    mountpoint: &std::path::Path,
) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (abort, mountpoint);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux HOST FUSE abort/detach is required",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        // Aborting alone leaves warm kernel attributes usable. Detach the actual
        // mount as well. Held descriptors/users remain the caller's lifecycle
        // responsibility; exporting bind/namespace view aliases is forbidden.
        // Detach before abort wakes the failed syscall: after it returns EIO,
        // its mount pathname must no longer expose long-TTL cached attributes.
        finish_termination(abort, || detach(mountpoint))
    }
}

#[cfg(target_os = "linux")]
fn finish_termination(
    abort: &std::fs::File,
    detach: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write;
    detach()?;
    let mut control = abort;
    control.write_all(b"1").or_else(|error| {
        if error.raw_os_error() == Some(libc::ENODEV) {
            Ok(())
        } else {
            Err(error)
        }
    })
}

pub(crate) fn detach(mountpoint: &std::path::Path) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mountpoint;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux detach required",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let path = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            mountpoint.as_os_str(),
        ))?;
        if unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) } == 0 {
            return verify_detached(mountpoint);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
            return verify_detached(mountpoint);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        for helper in [
            "fusermount3",
            "fusermount",
            "/bin/fusermount3",
            "/bin/fusermount",
        ] {
            if run_helper(helper, mountpoint, deadline).is_ok()
                && verify_detached(mountpoint).is_ok()
            {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        Err(io::Error::other(format!(
            "cannot detach FUSE mount {} within deadline",
            mountpoint.display()
        )))
    }
}

#[cfg(target_os = "linux")]
fn verify_detached(mountpoint: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut escaped = Vec::new();
    for byte in mountpoint.as_os_str().as_bytes() {
        match byte {
            b' ' => escaped.extend_from_slice(b"\\040"),
            b'\t' => escaped.extend_from_slice(b"\\011"),
            b'\n' => escaped.extend_from_slice(b"\\012"),
            b'\\' => escaped.extend_from_slice(b"\\134"),
            byte => escaped.push(*byte),
        }
    }
    let info = std::fs::read("/proc/self/mountinfo")?;
    if info
        .split(|byte| *byte == b'\n')
        .any(|line| line.split(|byte| *byte == b' ').nth(4) == Some(escaped.as_slice()))
    {
        return Err(io::Error::other(
            "mount is still present after detach attempt",
        ));
    }
    Ok(())
}

// Bounds opaque fuser mount/unmount calls too; no user callback is run here.
// Process termination is a containment signal, not revocation of external FDs.
pub(crate) struct LifecycleDeadline {
    done: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl LifecycleDeadline {
    pub(crate) fn start(timeout: std::time::Duration) -> Self {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let wait = done.clone();
        let worker = std::thread::spawn(move || {
            let guard = wait.0.lock().unwrap();
            let (guard, _) = wait
                .1
                .wait_timeout_while(guard, timeout, |done| !*done)
                .unwrap();
            if !*guard {
                std::process::abort();
            }
        });
        Self {
            done,
            worker: Some(worker),
        }
    }
}
impl Drop for LifecycleDeadline {
    fn drop(&mut self) {
        *self.done.0.lock().unwrap() = true;
        self.done.1.notify_all();
        let _ = self.worker.take().unwrap().join();
    }
}

#[cfg(target_os = "linux")]
fn run_helper(
    helper: &str,
    mountpoint: &std::path::Path,
    deadline: std::time::Instant,
) -> io::Result<()> {
    use std::process::{Command, Stdio};
    // No output pipes: a stuck helper or a descendant retaining them cannot
    // make output()/wait_with_output() block forever.
    let mut child = Command::new(helper)
        .args(["-u", "-z", "--"])
        .arg(mountpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("detach helper failed"))
            };
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            // Reap normally killed helpers, without an unbounded D-state wait.
            let reap_deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
            while std::time::Instant::now() < reap_deadline {
                if child.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "detach helper deadline exceeded",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;
    use std::time::Duration;

    struct Backend {
        log: Mutex<Vec<String>>,
        fail: AtomicBool,
        entry_fail: AtomicBool,
        gate: (Mutex<bool>, Condvar),
    }
    impl NotificationBackend for Backend {
        fn attributes(&self, ino: u64) -> io::Result<()> {
            self.log.lock().unwrap().push(format!("attr:{ino}"));
            if self.fail.load(Ordering::SeqCst) {
                Err(io::Error::from_raw_os_error(libc::EIO))
            } else {
                Ok(())
            }
        }
        fn entry(&self, parent: u64, _: &std::ffi::OsStr) -> io::Result<()> {
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                open = self.gate.1.wait(open).unwrap();
            }
            self.log.lock().unwrap().push(format!("entry:{parent}"));
            if self.entry_fail.load(Ordering::SeqCst) {
                Err(io::Error::from_raw_os_error(libc::ENOSYS))
            } else {
                Ok(())
            }
        }
    }
    fn backend(fail: bool) -> Arc<Backend> {
        Arc::new(Backend {
            log: Mutex::new(Vec::new()),
            fail: AtomicBool::new(fail),
            entry_fail: AtomicBool::new(false),
            gate: (Mutex::new(false), Condvar::new()),
        })
    }
    #[test]
    fn pending_reservations_are_globally_bounded_and_stop_admission() {
        let (handle, workers) =
            CacheWorkers::start_backend(backend(false), Box::new(|| Ok(()))).unwrap();
        for _ in 0..PENDING_LIMIT {
            assert!(handle.begin());
        }
        assert!(!handle.begin());
        assert!(handle.stopped());
        for _ in 0..PENDING_LIMIT {
            assert!(!handle.begin());
        }
        assert_eq!(handle.state.pending.load(Ordering::SeqCst), PENDING_LIMIT);
        drop(handle);
        assert!(workers.join().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_detach_never_writes_abort_endpoint() {
        use std::io::{Read, Seek};
        let mut endpoint = tempfile::tempfile().unwrap();
        assert!(
            finish_termination(&endpoint, || Err(io::Error::from_raw_os_error(libc::EPERM)))
                .is_err()
        );
        endpoint.rewind().unwrap();
        let mut bytes = Vec::new();
        endpoint.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
        finish_termination(&endpoint, || Ok(())).unwrap();
        endpoint.rewind().unwrap();
        endpoint.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detach_helper_deadline_is_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("stuck-helper");
        std::fs::write(&helper, b"#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = std::time::Instant::now();
        let error = run_helper(
            helper.to_str().unwrap(),
            root.path(),
            start + Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn termination_deadline_aborts_a_stuck_control_worker() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "PVISOR_OVERLAYFS_DEADLINE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            unsafe {
                libc::setrlimit(libc::RLIMIT_CORE, &limit);
            }
            let (_, workers) = CacheWorkers::start_backend(
                backend(false),
                Box::new(|| {
                    std::thread::sleep(Duration::from_secs(60));
                    Ok(())
                }),
            )
            .unwrap();
            workers.state.fail("injected stuck termination");
            panic!("deadline did not terminate process");
        }
        let start = std::time::Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache::tests::termination_deadline_aborts_a_stuck_control_worker",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGABRT));
        assert!(start.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn attributes_precede_reply_and_blocked_entries_never_block_related_replies() {
        let backend = backend(false);
        let (handle, workers) =
            CacheWorkers::start_backend(backend.clone(), Box::new(|| Ok(()))).unwrap();
        for ino in [2, 3] {
            assert!(handle.begin());
            let mut effects = Effects::default();
            effects.inodes.insert(ino);
            effects.entries.insert((1, "name".into()));
            let (tx, rx) = mpsc::channel();
            let log = backend.clone();
            handle.submit(effects, move |ok| {
                log.log.lock().unwrap().push(format!("reply:{ino}"));
                tx.send(ok).unwrap();
            });
            assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        assert!(handle.pending());
        assert_eq!(
            *backend.log.lock().unwrap(),
            ["attr:2", "reply:2", "attr:3", "reply:3"]
        );
        *backend.gate.0.lock().unwrap() = true;
        backend.gate.1.notify_all();
        drop(handle);
        workers.join().unwrap();
    }
    #[test]
    fn shutdown_ignores_only_disconnected_notifications_not_live_transport_errors() {
        let backend = backend(false);
        let (abort_tx, abort_rx) = mpsc::channel();
        let (handle, workers) = CacheWorkers::start_backend(
            backend,
            Box::new(move || {
                abort_tx.send(()).unwrap();
                Ok(())
            }),
        )
        .unwrap();
        workers.shutdown();
        workers
            .state
            .notification_error(io::Error::from_raw_os_error(libc::ENODEV));
        assert!(!workers.state.failed.load(Ordering::SeqCst));
        workers
            .state
            .notification_error(io::Error::from_raw_os_error(libc::EIO));
        abort_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(workers.state.failed.load(Ordering::SeqCst));
        drop(handle);
        assert!(workers.join().is_err());
    }

    #[test]
    fn entry_failure_after_reply_aborts_instead_of_dropping_effects() {
        let backend = backend(false);
        *backend.gate.0.lock().unwrap() = true;
        backend.entry_fail.store(true, Ordering::SeqCst);
        let (abort_tx, abort_rx) = mpsc::channel();
        let (handle, workers) = CacheWorkers::start_backend(
            backend,
            Box::new(move || {
                abort_tx.send(()).unwrap();
                Ok(())
            }),
        )
        .unwrap();
        assert!(handle.begin());
        let mut effects = Effects::default();
        effects.entries.insert((1, "new".into()));
        let (tx, rx) = mpsc::channel();
        handle.submit(effects, move |valid| tx.send(valid).unwrap());
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        abort_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(handle.pending()); // failed state never permits another long-TTL reply
        drop(handle);
        assert!(workers.join().is_err());
    }

    #[test]
    fn saturated_entry_queue_aborts_without_blocking_related_replies() {
        let backend = backend(false);
        let (abort_tx, abort_rx) = mpsc::channel();
        let (handle, workers) = CacheWorkers::start_backend(
            backend.clone(),
            Box::new(move || {
                abort_tx.send(()).unwrap();
                Ok(())
            }),
        )
        .unwrap();
        for _ in 0..QUEUE_LIMIT + 8 {
            if !handle.begin() {
                break;
            }
            let mut effects = Effects::default();
            effects.entries.insert((1, "new".into()));
            let (tx, rx) = mpsc::channel();
            handle.submit(effects, move |valid| tx.send(valid).unwrap());
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        abort_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!handle.begin(), "failure stops mutation admission");
        *backend.gate.0.lock().unwrap() = true;
        backend.gate.1.notify_all();
        drop(handle);
        assert!(workers.join().unwrap_err().to_string().contains("queue"));
    }

    #[test]
    fn saturated_reply_queue_submits_without_waiting_and_replies_after_termination() {
        struct BlockedAttributes {
            entered: mpsc::Sender<()>,
            gate: (Mutex<bool>, Condvar),
        }
        impl NotificationBackend for BlockedAttributes {
            fn attributes(&self, _: u64) -> io::Result<()> {
                self.entered.send(()).unwrap();
                let mut open = self.gate.0.lock().unwrap();
                while !*open {
                    open = self.gate.1.wait(open).unwrap();
                }
                Ok(())
            }
            fn entry(&self, _: u64, _: &std::ffi::OsStr) -> io::Result<()> {
                Ok(())
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let backend = Arc::new(BlockedAttributes {
            entered: entered_tx,
            gate: (Mutex::new(false), Condvar::new()),
        });
        let termination = Arc::new((Mutex::new(false), Condvar::new()));
        let stop_gate = termination.clone();
        let (stop_tx, stop_rx) = mpsc::channel();
        let (handle, workers) = CacheWorkers::start_backend(
            backend.clone(),
            Box::new(move || {
                stop_tx.send(()).unwrap();
                let mut done = stop_gate.0.lock().unwrap();
                while !*done {
                    done = stop_gate.1.wait(done).unwrap();
                }
                Ok(())
            }),
        )
        .unwrap();
        let (reply_tx, reply_rx) = mpsc::channel();
        assert!(handle.begin());
        let mut effects = Effects::default();
        effects.inodes.insert(2);
        let first_tx = reply_tx.clone();
        handle.submit(effects, move |valid| first_tx.send(valid).unwrap());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Keep the reply worker blocked, fill its normal queue, then overflow
        // from a callback-equivalent thread. The stop worker is also blocked.
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let submit_handle = handle.clone();
        let submitter = std::thread::spawn(move || {
            for _ in 0..QUEUE_LIMIT + 1 {
                assert!(submit_handle.begin());
                let tx = reply_tx.clone();
                submit_handle.submit(Effects::default(), move |valid| tx.send(valid).unwrap());
            }
            submitted_tx.send(()).unwrap();
        });
        submitted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        stop_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(reply_rx.try_recv().is_err());
        *backend.gate.0.lock().unwrap() = true;
        backend.gate.1.notify_all();
        assert!(reply_rx.recv_timeout(Duration::from_millis(50)).is_err());
        *termination.0.lock().unwrap() = true;
        termination.1.notify_all();
        for _ in 0..QUEUE_LIMIT + 2 {
            assert!(!reply_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        submitter.join().unwrap();
        drop(handle);
        assert!(workers.join().unwrap_err().to_string().contains("queue"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires real HOST FUSE and fusectl abort permission"]
    fn host_kernel_cache_injected_notification_failure_aborts_detaches_and_preserves_effects() {
        use crate::api::{KernelCacheConfig, KernelCachePolicy};
        use crate::fs::OverlayFs;
        use std::fs::{self, File, FileTimes, OpenOptions};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::time::SystemTime;
        struct Inject {
            notifier: Notifier,
            fail: AtomicBool,
        }
        impl NotificationBackend for Inject {
            fn attributes(&self, ino: u64) -> io::Result<()> {
                if self.fail.load(Ordering::SeqCst) {
                    Err(io::Error::from_raw_os_error(libc::EIO))
                } else {
                    self.notifier.inval_inode(ino, -1, 0)
                }
            }
            fn entry(&self, parent: u64, name: &std::ffi::OsStr) -> io::Result<()> {
                self.notifier.inval_entry(parent, name)
            }
        }
        // Exercise the private transport with an injected OS notification error,
        // not an environment-enabled cache path or a public testing escape hatch.
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let lower = root.path().join("lower");
        let upper = root.path().join("upper");
        let mountpoint = root.path().join("merged");
        for path in [&lower, &upper, &mountpoint] {
            fs::create_dir(path).unwrap();
        }
        fs::write(lower.join("a"), b"original").unwrap();
        let future = SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60);
        for path in [&lower, &upper, &lower.join("a")] {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_accessed(future))
                .unwrap();
        }
        let config = KernelCacheConfig {
            policy: KernelCachePolicy::Metadata,
            ..KernelCacheConfig::default()
        };
        let filesystem = OverlayFs::new(vec![lower.clone()], upper.clone(), None)
            .unwrap()
            .with_kernel_cache(config, vec![]);
        let slot = filesystem.cache_slot();
        let session = fuser::Session::new(
            filesystem,
            &mountpoint,
            &[
                fuser::MountOption::DefaultPermissions,
                fuser::MountOption::NoAtime,
            ],
        )
        .unwrap();
        let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap();
        let device = mountinfo
            .lines()
            .find_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.get(4).copied() == mountpoint.to_str()).then(|| fields[2].to_owned())
            })
            .unwrap();
        let id: u32 = device.strip_prefix("0:").unwrap().parse().unwrap();
        let abort = OpenOptions::new()
            .write(true)
            .open(format!("/sys/fs/fuse/connections/{id}/abort"))
            .unwrap();
        let backend = Arc::new(Inject {
            notifier: session.notifier(),
            fail: AtomicBool::new(false),
        });
        let detached_path = mountpoint.clone();
        let (handle, workers) = CacheWorkers::start_backend(
            backend.clone(),
            Box::new(move || abort_and_detach(&abort, &detached_path)),
        )
        .unwrap();
        assert!(slot.set(handle).is_ok());
        drop(slot);
        let background = fuser::BackgroundSession::new(session).unwrap();
        let path = mountpoint.join("a");
        fs::metadata(&path).unwrap(); // publish real 60-second attributes
        backend.fail.store(true, Ordering::SeqCst);
        assert!(fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).is_err());
        assert!(
            !mountpoint.join("a").exists(),
            "notification failure must detach warm-cache mount"
        );
        assert_eq!(
            fs::metadata(upper.join("a")).unwrap().mode() & 0o777,
            0o600,
            "mutation effects are not rolled back or silently discarded"
        );
        background.unmount().unwrap();
        assert!(
            workers
                .join()
                .unwrap_err()
                .to_string()
                .contains("invalidation failed")
        );
        assert_eq!(fs::read(lower.join("a")).unwrap(), b"original");
    }

    #[test]
    fn notification_failure_aborts_and_does_not_send_success() {
        let backend = backend(true);
        let aborted = Arc::new(AtomicBool::new(false));
        let flag = aborted.clone();
        let (handle, workers) = CacheWorkers::start_backend(
            backend.clone(),
            Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }),
        )
        .unwrap();
        assert!(handle.begin());
        let mut effects = Effects::default();
        effects.inodes.insert(2);
        let (tx, rx) = mpsc::channel();
        handle.submit(effects, move |ok| tx.send(ok).unwrap());
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(aborted.load(Ordering::SeqCst));
        drop(handle);
        assert!(
            workers
                .join()
                .unwrap_err()
                .to_string()
                .contains("invalidation failed")
        );
    }
}

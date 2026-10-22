//! Separate bounded read and mutation workers; preparation releases state.
use crate::fs::{OverlayFs, setattr_requires_copy_up};
#[cfg(test)]
use fuser::FUSE_ROOT_ID;
#[cfg(target_os = "macos")]
use fuser::ReplyXTimes;
use fuser::{
    Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty,
    ReplyEntry, ReplyLseek, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use pvisor_overlay_core::{OverlayCore, PreparedCopyUp, service::FilesystemService};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::SystemTime;

const REQUEST_LIMIT: usize = 128;
const BYTE_LIMIT: usize = 16 * 1024 * 1024;
type Callback = Box<dyn FnOnce(Result<&mut OverlayFs, i32>) + Send>;

#[derive(Default)]
struct IoActivity {
    state: Mutex<(usize, bool)>,
    ready: Condvar,
}

impl IoActivity {
    fn enter(&self) -> IoPermit<'_> {
        let mut state = self.state.lock().unwrap();
        while state.1 {
            state = self.ready.wait(state).unwrap();
        }
        state.0 += 1;
        IoPermit(self)
    }

    fn namespace(&self) -> NamespacePermit<'_> {
        let mut state = self.state.lock().unwrap();
        while state.1 {
            state = self.ready.wait(state).unwrap();
        }
        state.1 = true;
        while state.0 != 0 {
            state = self.ready.wait(state).unwrap();
        }
        NamespacePermit(self)
    }
}

struct IoPermit<'a>(&'a IoActivity);

impl Drop for IoPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.0 -= 1;
        if state.0 == 0 {
            self.0.ready.notify_all();
        }
    }
}

struct NamespacePermit<'a>(&'a IoActivity);

impl Drop for NamespacePermit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().1 = false;
        self.0.ready.notify_all();
    }
}

enum IoRequest<'a> {
    Read {
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        reply: ReplyData,
    },
    Write {
        ino: u64,
        fh: u64,
        offset: i64,
        data: Cow<'a, [u8]>,
        flags: i32,
        reply: ReplyWrite,
    },
    Sync {
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    },
}

impl IoRequest<'_> {
    fn into_owned(self) -> IoRequest<'static> {
        match self {
            Self::Read {
                ino,
                fh,
                offset,
                size,
                reply,
            } => IoRequest::Read {
                ino,
                fh,
                offset,
                size,
                reply,
            },
            Self::Write {
                ino,
                fh,
                offset,
                data,
                flags,
                reply,
            } => IoRequest::Write {
                ino,
                fh,
                offset,
                data: Cow::Owned(data.into_owned()),
                flags,
                reply,
            },
            Self::Sync {
                fh,
                datasync,
                reply,
            } => IoRequest::Sync {
                fh,
                datasync,
                reply,
            },
        }
    }

    fn fail(self, error: i32) {
        match self {
            Self::Read { reply, .. } => reply.error(error),
            Self::Write { reply, .. } => reply.error(error),
            Self::Sync { reply, .. } => reply.error(error),
        }
    }

    fn run(self, state: &Mutex<OverlayFs>, activity: &IoActivity) {
        // This is an in-flight counter/fence, not a mutex held through I/O.
        // Namespace publication waits here before acquiring filesystem state.
        let _permit = activity.enter();
        match self {
            Self::Read {
                ino,
                fh,
                offset,
                size,
                reply,
            } => {
                let lease = state.lock().map_err(|_| libc::EIO).and_then(|fs| {
                    fs.begin_file_io(ino, fh, offset, false)
                        .map(|(lease, _)| lease)
                        .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
                });
                let lease = match lease {
                    Ok(lease) => lease,
                    Err(error) => {
                        reply.error(error);
                        return;
                    }
                };
                let result = lease.read(offset as u64, size);
                match state.lock() {
                    Ok(fs) => fs.finish_read_io(&lease, &result),
                    Err(_) => {
                        reply.error(libc::EIO);
                        return;
                    }
                }
                match result {
                    Ok(bytes) => reply.data(&bytes),
                    Err(error) => reply.error(error.raw_os_error().unwrap_or(libc::EIO)),
                }
            }
            Self::Write {
                ino,
                fh,
                offset,
                data,
                flags,
                reply,
            } => {
                let prepared = state.lock().map_err(|_| libc::EIO).and_then(|fs| {
                    fs.begin_file_io(ino, fh, offset, true)
                        .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
                });
                let (lease, mutation) = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        reply.error(error);
                        return;
                    }
                };
                let result = lease.write(offset as u64, &data, flags);
                match state.lock() {
                    Ok(mut fs) => fs.finish_write_io(&lease, mutation, result, reply),
                    Err(poisoned) => {
                        // Complete the reserved cache effects even after a state
                        // failure; the backing write may already have happened.
                        poisoned.into_inner().finish_write_io(
                            &lease,
                            mutation,
                            Err(io::Error::from_raw_os_error(libc::EIO)),
                            reply,
                        );
                    }
                }
            }
            Self::Sync {
                fh,
                datasync,
                reply,
            } => {
                let lease = state.lock().map_err(|_| libc::EIO).and_then(|fs| {
                    fs.file_io_lease(fh)
                        .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
                });
                match lease.and_then(|lease| {
                    lease
                        .sync(datasync)
                        .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
                }) {
                    Ok(()) => reply.ok(),
                    Err(error) => reply.error(error),
                }
            }
        }
    }
}

enum Work {
    Callback(Callback),
    Io(IoRequest<'static>),
}

impl Work {
    fn fail(self, error: i32) {
        match self {
            Self::Callback(callback) => callback(Err(error)),
            Self::Io(request) => request.fail(error),
        }
    }
}

pub(super) enum CopyRequest {
    None,
    Inode(u64),
    Open {
        ino: u64,
        flags: i32,
    },
    Setattr {
        ino: u64,
        fh: Option<u64>,
    },
    Unlink {
        parent: u64,
        name: OsString,
    },
    #[cfg(test)]
    Paths(Vec<PathBuf>),
    Rename {
        parent: u64,
        name: OsString,
        newparent: u64,
        newname: OsString,
        flags: u32,
    },
}

pub(super) enum CopyPlan {
    Files(Vec<PathBuf>, bool),
    Rename {
        old: PathBuf,
        new: PathBuf,
        flags: u32,
        open_paths: Vec<PathBuf>,
    },
}

fn prepare(core: &FilesystemService, plan: CopyPlan) -> io::Result<Vec<PreparedCopyUp>> {
    let (mut paths, truncate, roots) = match plan {
        CopyPlan::Files(paths, truncate) => (paths, truncate, Vec::new()),
        CopyPlan::Rename {
            old,
            new,
            flags,
            open_paths,
        } => {
            let roots = core.prepare_rename_copy_up(&old, &new, flags & 1 != 0, flags & 2 != 0)?;
            (open_paths, false, roots)
        }
    };
    // Walk merged names before any upper publication, including lower children
    // of existing upper directories. Never follow symlinks into another tree.
    let mut pending = roots;
    while let Some(path) = pending.pop() {
        if core.metadata(&path)?.is_dir() {
            for (name, _) in core.directory_candidates(&path)? {
                pending.push(OverlayCore::child(&path, &name)?);
            }
        } else {
            paths.push(path);
        }
    }
    core.prepare_copy_ups(
        &paths
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
        truncate,
    )
}

struct Job {
    preparation: CopyRequest,
    bytes: usize,
    inodes: Vec<u64>,
    work: Work,
}

fn failed_preparation(
    fs: &mut OverlayFs,
    mutation: Option<crate::fs::Mutation>,
    callback: Callback,
    error: i32,
) {
    fs.finish_copy_preparation(mutation, move |valid| {
        callback(Err(if valid { error } else { libc::EIO }));
    });
}

fn complete_preparation(
    fs: &mut OverlayFs,
    core: &FilesystemService,
    copies: Result<Vec<PreparedCopyUp>, i32>,
    mutation: Option<crate::fs::Mutation>,
    callback: Callback,
) {
    let copies = match copies {
        Ok(copies) => copies,
        Err(error) => {
            failed_preparation(fs, mutation, callback, error);
            return;
        }
    };
    let prepared = match core.use_prepared_copy_ups(copies) {
        Ok(prepared) => prepared,
        Err(error) => {
            failed_preparation(
                fs,
                mutation,
                callback,
                error.raw_os_error().unwrap_or(libc::EIO),
            );
            return;
        }
    };
    // Keep pending through unused-copy cleanup, while the first invalidation
    // precedes the original operation's replies.
    let cleanup = match fs.begin_copy_preparation(mutation.is_some()) {
        Ok(cleanup) => cleanup,
        Err(error) => {
            drop(prepared);
            failed_preparation(
                fs,
                mutation,
                callback,
                error.raw_os_error().unwrap_or(libc::EIO),
            );
            return;
        }
    };
    fs.finish_copy_preparation(mutation, |_| {});
    callback(Ok(fs));
    drop(prepared);
    fs.finish_copy_preparation(cleanup, |_| {});
}

#[derive(Debug)]
pub(crate) struct DispatchControl {
    stopped: AtomicBool,
    senders: Mutex<Vec<mpsc::SyncSender<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl DispatchControl {
    pub(crate) fn shutdown(&self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.senders
            .lock()
            .map_err(|_| io::Error::other("request admission poisoned"))?
            .clear();
        let mut result = Ok(());
        for worker in self
            .workers
            .lock()
            .map_err(|_| io::Error::other("request worker poisoned"))?
            .drain(..)
        {
            if worker.join().is_err() {
                result = Err(io::Error::other("overlay request worker panicked"));
            }
        }
        result
    }
}

pub(crate) struct DispatchFs {
    state: Arc<Mutex<OverlayFs>>,
    io_activity: Arc<IoActivity>,
    pub(crate) control: Arc<DispatchControl>,
}

impl DispatchFs {
    pub(crate) fn new(filesystem: OverlayFs) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(filesystem));
        let io_activity = Arc::new(IoActivity::default());
        let worker_state = state.clone();
        let mutation_activity = io_activity.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(REQUEST_LIMIT);
        let worker = std::thread::Builder::new()
            .name("overlay-mutations".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let callback = match job.work {
                        Work::Callback(callback) => callback,
                        Work::Io(request) => {
                            request.run(&worker_state, &mutation_activity);
                            if let Ok(mut fs) = worker_state.lock() {
                                fs.finish_request(job.bytes, &job.inodes);
                            }
                            continue;
                        }
                    };
                    let result = (|| {
                        let (core, plan, mutation) = {
                            let fs = worker_state.lock().map_err(|_| libc::EIO)?;
                            let (core, plan) = fs
                                .copy_plan(&job.preparation)
                                .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
                            let needed =
                                !matches!(&plan, CopyPlan::Files(paths, _) if paths.is_empty());
                            let mutation = fs
                                .begin_copy_preparation(needed)
                                .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
                            (core, plan, mutation)
                        };
                        let copies = prepare(&core, plan)
                            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO));
                        Ok((core, copies, mutation))
                    })();
                    let _namespace = mutation_activity.namespace();
                    match worker_state.lock() {
                        Ok(mut fs) => {
                            match result {
                                Ok((core, copies, mutation)) => {
                                    complete_preparation(&mut fs, &core, copies, mutation, callback)
                                }
                                Err(error) => callback(Err(error)),
                            }
                            fs.finish_request(job.bytes, &job.inodes);
                        }
                        Err(_) => callback(Err(libc::EIO)),
                    }
                }
            })?;
        let read_state = state.clone();
        let read_activity = io_activity.clone();
        let (read_sender, read_receiver) = mpsc::sync_channel::<Job>(REQUEST_LIMIT);
        let read_worker = match std::thread::Builder::new()
            .name("overlay-reads".into())
            .spawn(move || {
                while let Ok(job) = read_receiver.recv() {
                    match job.work {
                        Work::Io(request) => request.run(&read_state, &read_activity),
                        Work::Callback(callback) => {
                            let _permit = read_activity.enter();
                            match read_state.lock() {
                                Ok(mut fs) => callback(Ok(&mut fs)),
                                Err(_) => callback(Err(libc::EIO)),
                            }
                        }
                    }
                    if let Ok(mut fs) = read_state.lock() {
                        fs.pending_read_requests -= 1;
                        fs.finish_request(job.bytes, &job.inodes);
                    }
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                drop(sender);
                let _ = worker.join();
                return Err(error);
            }
        };
        Ok(Self {
            state,
            io_activity,
            control: Arc::new(DispatchControl {
                stopped: AtomicBool::new(false),
                senders: Mutex::new(vec![sender, read_sender]),
                workers: Mutex::new(vec![worker, read_worker]),
            }),
        })
    }

    fn metadata(&self, callback: impl FnOnce(Result<&mut OverlayFs, i32>)) {
        if self.control.stopped.load(Ordering::Acquire) {
            callback(Err(libc::EIO));
        } else {
            match self.state.lock() {
                Ok(mut fs) => callback(Ok(&mut fs)),
                Err(_) => callback(Err(libc::EIO)),
            }
        }
    }

    fn xattr_read(&self, ino: u64, callback: Callback) {
        if self.control.stopped.load(Ordering::Acquire) {
            callback(Err(libc::EIO));
            return;
        }
        match self.state.lock() {
            Ok(mut fs) if fs.is_directory_inode(ino) => {
                // macOS stat probes parent FinderInfo; directory observations
                // have no regular-file contents to fingerprint.
                callback(Ok(&mut fs));
            }
            Ok(fs) => {
                drop(fs);
                self.enqueue_read(&[ino], callback);
            }
            Err(_) => callback(Err(libc::EIO)),
        }
    }

    fn enqueue(&self, preparation: CopyRequest, bytes: usize, inodes: &[u64], callback: Callback) {
        let short = matches!(preparation, CopyRequest::None);
        self.enqueue_on(false, short, preparation, bytes, inodes, callback);
    }

    fn enqueue_read(&self, inodes: &[u64], callback: Callback) {
        self.enqueue_on(true, true, CopyRequest::None, 0, inodes, callback);
    }

    fn enqueue_open(&self, ino: u64, flags: i32, callback: Callback) {
        if FilesystemService::is_write_open(flags) {
            self.enqueue(CopyRequest::Open { ino, flags }, 0, &[ino], callback);
        } else {
            self.enqueue_read(&[ino], callback);
        }
    }

    fn enqueue_handle(&self, ino: u64, fh: u64, callback: Callback) {
        if self.control.stopped.load(Ordering::Acquire) {
            callback(Err(libc::EIO));
            return;
        }
        let reading = match self.state.lock() {
            Ok(fs) => fs.is_readonly_handle(fh),
            Err(_) => {
                callback(Err(libc::EIO));
                return;
            }
        };
        if reading {
            self.enqueue_read(&[ino], callback);
        } else {
            self.enqueue(CopyRequest::None, 0, &[ino], callback);
        }
    }

    fn enqueue_io(
        &self,
        reading: bool,
        short: bool,
        bytes: usize,
        ino: u64,
        request: IoRequest<'_>,
    ) {
        let Ok(senders) = self.control.senders.lock() else {
            request.fail(libc::EIO);
            return;
        };
        if self.control.stopped.load(Ordering::Acquire) {
            request.fail(libc::EIO);
            return;
        }
        let Some(sender) = senders.get(usize::from(reading)) else {
            request.fail(libc::EIO);
            return;
        };
        let Ok(mut fs) = self.state.lock() else {
            request.fail(libc::EIO);
            return;
        };
        let pending = if reading {
            fs.pending_read_requests
        } else {
            fs.pending_requests - fs.pending_read_requests
        };
        if pending >= REQUEST_LIMIT || bytes > BYTE_LIMIT - fs.pending_bytes {
            drop(fs);
            request.fail(libc::EAGAIN);
            return;
        }
        *fs.pending_inodes.entry(ino).or_default() += 1;
        fs.pending_requests += 1;
        fs.pending_read_requests += usize::from(reading);
        fs.pending_bytes += bytes;
        if short && pending == 0 && (!reading || fs.pending_requests == 1) {
            drop(fs);
            // Keep admission locked through inline completion so shutdown drains
            // it too. A borrowed WRITE payload needs no allocation or copy.
            request.run(&self.state, &self.io_activity);
            let mut fs = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            fs.pending_read_requests -= usize::from(reading);
            fs.finish_request(bytes, &[ino]);
            return;
        }
        // Copy only an admitted, actually queued WRITE. Rejections and idle
        // inline writes never allocate a payload or a boxed callback.
        let job = Job {
            preparation: CopyRequest::None,
            bytes,
            inodes: vec![ino],
            work: Work::Io(request.into_owned()),
        };
        if let Err(error) = sender.try_send(job) {
            let (job, errno) = match error {
                mpsc::TrySendError::Full(job) => (job, libc::EAGAIN),
                mpsc::TrySendError::Disconnected(job) => (job, libc::EIO),
            };
            fs.pending_read_requests -= usize::from(reading);
            fs.finish_request(bytes, &[ino]);
            drop(fs);
            job.work.fail(errno);
        }
    }

    fn enqueue_on(
        &self,
        reading: bool,
        short: bool,
        preparation: CopyRequest,
        bytes: usize,
        inodes: &[u64],
        callback: Callback,
    ) {
        let job = Job {
            preparation,
            bytes,
            inodes: inodes.to_owned(),
            work: Work::Callback(callback),
        };
        let Ok(senders) = self.control.senders.lock() else {
            job.work.fail(libc::EIO);
            return;
        };
        if self.control.stopped.load(Ordering::Acquire) {
            job.work.fail(libc::EIO);
            return;
        }
        let Some(sender) = senders.get(usize::from(reading)) else {
            job.work.fail(libc::EIO);
            return;
        };
        let Ok(mut fs) = self.state.lock() else {
            job.work.fail(libc::EIO);
            return;
        };
        let pending = if reading {
            fs.pending_read_requests
        } else {
            fs.pending_requests - fs.pending_read_requests
        };
        if pending >= REQUEST_LIMIT || bytes > BYTE_LIMIT - fs.pending_bytes {
            drop(fs);
            job.work.fail(libc::EAGAIN);
            return;
        }
        for ino in &job.inodes {
            *fs.pending_inodes.entry(*ino).or_default() += 1;
        }
        fs.pending_requests += 1;
        if reading {
            fs.pending_read_requests += 1;
        }
        fs.pending_bytes += bytes;
        // These handlers execute under the state lock. When
        // their lane is idle, a worker handoff only adds scheduling latency.
        // Keep admission, inode pins and the sender lock through completion:
        // shutdown must also wait for an inline callback. Reads may be inline
        // only when *both* lanes were idle: any accepted mutation, including
        // off-lock copy preparation, keeps the receive thread free for metadata.
        // Never overtake an accepted job in the same lane.
        if short && pending == 0 && (!reading || fs.pending_requests == 1) {
            drop(fs);
            let (read_permit, namespace_permit) = if reading {
                (Some(self.io_activity.enter()), None)
            } else {
                (None, Some(self.io_activity.namespace()))
            };
            let mut fs = match self.state.lock() {
                Ok(fs) => fs,
                Err(_) => {
                    job.work.fail(libc::EIO);
                    return;
                }
            };
            if let Work::Callback(callback) = job.work {
                callback(Ok(&mut fs));
            }
            if reading {
                fs.pending_read_requests -= 1;
            }
            fs.finish_request(bytes, &job.inodes);
            drop(fs);
            drop(read_permit);
            drop(namespace_permit);
            return;
        }
        if let Err(error) = sender.try_send(job) {
            let (job, errno) = match error {
                mpsc::TrySendError::Full(job) => (job, libc::EAGAIN),
                mpsc::TrySendError::Disconnected(job) => (job, libc::EIO),
            };
            if reading {
                fs.pending_read_requests -= 1;
            }
            fs.finish_request(bytes, &job.inodes);
            drop(fs);
            job.work.fail(errno);
        }
    }
}

impl Drop for DispatchFs {
    fn drop(&mut self) {
        let _ = self.control.shutdown();
    }
}

macro_rules! reply_callback {
    ($reply:ident, $method:ident($($args:expr),* $(,)?)) => {
        move |state: Result<&mut OverlayFs, i32>| match state {
            Ok(fs) => fs.$method($($args,)* $reply),
            Err(error) => $reply.error(error),
        }
    };
}

impl Filesystem for DispatchFs {
    fn forget(&mut self, _request: &Request<'_>, ino: u64, nlookup: u64) {
        self.metadata(|state| {
            if let Ok(fs) = state {
                fs.forget(ino, nlookup);
            }
        });
    }

    fn lookup(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_owned();
        self.metadata(reply_callback!(reply, lookup(parent, &name)));
    }

    fn getattr(&mut self, _request: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        self.metadata(reply_callback!(reply, getattr(ino, fh)));
    }

    fn setattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let mutating = setattr_requires_copy_up(mode, uid, gid, size, atime, mtime, flags);
        let callback: Callback = Box::new(reply_callback!(
            reply,
            setattr(
                ino, mode, uid, gid, size, atime, mtime, _ctime, fh, _crtime, _chgtime, _bkuptime,
                flags
            )
        ));
        if mutating {
            self.enqueue(CopyRequest::Setattr { ino, fh }, 0, &[ino], callback);
        } else {
            self.metadata(callback);
        }
    }

    fn readlink(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyData) {
        self.enqueue_read(&[ino], Box::new(reply_callback!(reply, readlink(ino))));
    }

    fn mknod(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let name = name.to_owned();
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(
                reply,
                mknod(parent, &name, mode, umask, rdev)
            )),
        );
    }

    fn mkdir(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let name = name.to_owned();
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(reply, mkdir(parent, &name, mode, umask))),
        );
    }

    fn unlink(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_owned();
        let preparation = CopyRequest::Unlink {
            parent,
            name: name.clone(),
        };
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(reply, unlink(parent, &name))),
        );
    }

    fn rmdir(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_owned();
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(reply, rmdir(parent, &name))),
        );
    }

    fn symlink(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let name = name.to_owned();
        let target = target.to_owned();
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(reply, symlink(parent, &name, &target))),
        );
    }

    fn rename(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let name = name.to_owned();
        let newname = newname.to_owned();
        let preparation = CopyRequest::Rename {
            parent,
            name: name.clone(),
            newparent,
            newname: newname.clone(),
            flags,
        };
        self.enqueue(
            preparation,
            0,
            &[parent, newparent],
            Box::new(reply_callback!(
                reply,
                rename(parent, &name, newparent, &newname, flags)
            )),
        );
    }

    fn link(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let newname = newname.to_owned();
        let preparation = CopyRequest::Inode(ino);
        self.enqueue(
            preparation,
            0,
            &[ino, newparent],
            Box::new(reply_callback!(reply, link(ino, newparent, &newname))),
        );
    }

    fn open(&mut self, _request: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        self.enqueue_open(
            ino,
            flags,
            Box::new(reply_callback!(reply, open(ino, flags))),
        );
    }

    fn read(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        // Preserve worker pipelining for bulk reads, even on an idle mount.
        self.enqueue_io(
            true,
            size < 64 * 1024,
            0,
            ino,
            IoRequest::Read {
                ino,
                fh,
                offset,
                size,
                reply,
            },
        );
    }

    fn write(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        self.enqueue_io(
            false,
            true,
            data.len(),
            ino,
            IoRequest::Write {
                ino,
                fh,
                offset,
                data: Cow::Borrowed(data),
                flags,
                reply,
            },
        );
    }

    fn flush(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        self.enqueue_handle(
            _ino,
            fh,
            Box::new(reply_callback!(reply, flush(_ino, fh, _lock_owner))),
        );
    }

    fn release(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.enqueue_handle(
            _ino,
            fh,
            Box::new(reply_callback!(
                reply,
                release(_ino, fh, _flags, _lock_owner, _flush)
            )),
        );
    }

    fn fsync(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        self.enqueue_io(
            false,
            true,
            0,
            _ino,
            IoRequest::Sync {
                fh,
                datasync,
                reply,
            },
        );
    }

    fn opendir(&mut self, _request: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        self.enqueue_read(
            &[ino],
            Box::new(reply_callback!(reply, opendir(ino, _flags))),
        );
    }

    fn readdir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        reply: ReplyDirectory,
    ) {
        self.metadata(reply_callback!(reply, readdir(_ino, fh, offset)));
    }

    fn readdirplus(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        reply: ReplyDirectoryPlus,
    ) {
        self.metadata(reply_callback!(reply, readdirplus(_ino, fh, offset)));
    }

    fn releasedir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        self.enqueue_read(
            &[_ino],
            Box::new(reply_callback!(reply, releasedir(_ino, fh, _flags))),
        );
    }

    fn fsyncdir(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        _fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[ino],
            Box::new(reply_callback!(reply, fsyncdir(ino, _fh, datasync))),
        );
    }

    fn statfs(&mut self, _request: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        self.metadata(reply_callback!(reply, statfs(_ino)));
    }

    fn setxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        let name = name.to_owned();
        let value = value.to_owned();
        let preparation =
            if position == 0 && pvisor_overlay_core::validate_guest_xattr(&name).is_ok() {
                CopyRequest::Inode(ino)
            } else {
                CopyRequest::None
            };
        self.enqueue(
            preparation,
            value.len(),
            &[ino],
            Box::new(reply_callback!(
                reply,
                setxattr(ino, &name, &value, flags, position)
            )),
        );
    }

    fn getxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let name = name.to_owned();
        self.xattr_read(
            ino,
            Box::new(reply_callback!(reply, getxattr(ino, &name, size))),
        );
    }

    fn listxattr(&mut self, _request: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        self.xattr_read(ino, Box::new(reply_callback!(reply, listxattr(ino, size))));
    }

    fn removexattr(&mut self, _request: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_owned();
        let preparation = if pvisor_overlay_core::validate_guest_xattr(&name).is_ok() {
            CopyRequest::Inode(ino)
        } else {
            CopyRequest::None
        };
        self.enqueue(
            preparation,
            0,
            &[ino],
            Box::new(reply_callback!(reply, removexattr(ino, &name))),
        );
    }

    fn access(&mut self, _request: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        self.metadata(reply_callback!(reply, access(ino, mask)));
    }

    fn create(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name = name.to_owned();
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[parent],
            Box::new(reply_callback!(
                reply,
                create(parent, &name, mode, umask, flags)
            )),
        );
    }

    fn fallocate(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[ino],
            Box::new(reply_callback!(
                reply,
                fallocate(ino, fh, offset, length, mode)
            )),
        );
    }

    fn lseek(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        self.enqueue_handle(
            _ino,
            fh,
            Box::new(reply_callback!(reply, lseek(_ino, fh, offset, whence))),
        );
    }

    fn copy_file_range(
        &mut self,
        _request: &Request<'_>,
        _ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        _ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        let preparation = CopyRequest::None;
        self.enqueue(
            preparation,
            0,
            &[],
            Box::new(reply_callback!(
                reply,
                copy_file_range(
                    _ino_in, fh_in, offset_in, _ino_out, fh_out, offset_out, len, flags
                )
            )),
        );
    }

    #[cfg(target_os = "macos")]
    fn exchange(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        let name = name.to_owned();
        let newname = newname.to_owned();
        let preparation = CopyRequest::Rename {
            parent,
            name: name.clone(),
            newparent,
            newname: newname.clone(),
            flags: 2,
        };
        self.enqueue(
            preparation,
            0,
            &[parent, newparent],
            Box::new(reply_callback!(
                reply,
                exchange(parent, &name, newparent, &newname, _options)
            )),
        );
    }

    #[cfg(target_os = "macos")]
    fn getxtimes(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyXTimes) {
        self.metadata(reply_callback!(reply, getxtimes(ino)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_overlay_core::backend::{self, BackendAttachment, ReadOnlyBackend};
    use std::fs;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::sync::mpsc::{Receiver, SyncSender};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn namespace_publication_waits_for_inflight_io_to_drain() {
        let activity = Arc::new(IoActivity::default());
        let io = activity.enter();
        let (sent, completed) = mpsc::channel();
        let other = activity.clone();
        let worker = std::thread::spawn(move || {
            let _namespace = other.namespace();
            sent.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if activity.state.lock().unwrap().1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(completed.try_recv().is_err());
        drop(io);
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        let _next = activity.enter();
    }

    struct SlowLower {
        root: PathBuf,
        first: AtomicBool,
        entered: SyncSender<()>,
        resume: Mutex<Receiver<()>>,
        fail: bool,
    }
    impl ReadOnlyBackend for SlowLower {
        fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn prepare_directory(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn attributes(&self, path: &Path) -> io::Result<backend::FileAttr> {
            let m = fs::symlink_metadata(self.root.join(path))?;
            Ok(backend::FileAttr {
                ino: m.ino(),
                size: m.len(),
                blocks: m.blocks(),
                atime: m.accessed()?,
                mtime: m.modified()?,
                ctime: UNIX_EPOCH,
                crtime: UNIX_EPOCH,
                kind: if m.is_dir() {
                    backend::FileType::Directory
                } else {
                    backend::FileType::RegularFile
                },
                perm: (m.mode() & 0o7777) as u16,
                uid: m.uid(),
                gid: m.gid(),
                nlink: m.nlink() as u32,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            })
        }
        fn read_at(&self, path: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>> {
            let mut bytes = vec![0; size as usize];
            let n = fs::File::open(self.root.join(path))?.read_at(&mut bytes, offset)?;
            bytes.truncate(n);
            Ok(bytes)
        }
        fn materialize_file(&self, path: &Path) -> io::Result<()> {
            if path == Path::new("slow") && self.first.swap(false, Ordering::AcqRel) {
                self.entered
                    .send(())
                    .map_err(|_| io::Error::other("test gate closed"))?;
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| io::Error::other("test gate timeout"))?;
                if self.fail {
                    return Err(io::Error::from_raw_os_error(libc::ENOSPC));
                }
            }
            Ok(())
        }
        fn materialize_tree(&self, _: &Path) -> io::Result<()> {
            unreachable!()
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        dispatch: Arc<DispatchFs>,
        _attachment: BackendAttachment,
        entered: Receiver<()>,
        resume: SyncSender<()>,
    }
    fn fixture(fail: bool) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        let lower = lower.canonicalize().unwrap();
        fs::write(lower.join("slow"), b"original").unwrap();
        fs::write(lower.join("quick"), b"metadata").unwrap();
        let overlay = OverlayFs::new(vec![lower.clone()], root.path().join("upper"), None).unwrap();
        let (entered, notification) = mpsc::sync_channel(1);
        let (resume, gate) = mpsc::sync_channel(1);
        let attachment = BackendAttachment::new(
            &lower,
            Arc::new(SlowLower {
                root: lower.clone(),
                first: AtomicBool::new(true),
                entered,
                resume: Mutex::new(gate),
                fail,
            }),
        )
        .unwrap();
        Fixture {
            root,
            dispatch: Arc::new(DispatchFs::new(overlay).unwrap()),
            _attachment: attachment,
            entered: notification,
            resume,
        }
    }
    fn copy_request() -> CopyRequest {
        CopyRequest::Paths(vec!["slow".into()])
    }
    fn service(fs: &OverlayFs) -> Arc<FilesystemService> {
        fs.copy_plan(&CopyRequest::None).unwrap().0
    }

    #[test]
    fn idle_short_callbacks_complete_without_a_worker_handoff() {
        let f = fixture(false);
        let caller = std::thread::current().id();
        for reading in [false, true] {
            let callback: Callback = Box::new(move |state| {
                let fs = state.unwrap();
                assert_eq!(std::thread::current().id(), caller);
                assert_eq!(fs.pending_inodes.get(&FUSE_ROOT_ID), Some(&1));
                assert_eq!(fs.pending_requests, 1);
                assert_eq!(fs.pending_read_requests, usize::from(reading));
            });
            if reading {
                f.dispatch.enqueue_read(&[FUSE_ROOT_ID], callback);
            } else {
                f.dispatch
                    .enqueue(CopyRequest::None, 0, &[FUSE_ROOT_ID], callback);
            }
            let state = f.dispatch.state.lock().unwrap();
            assert_eq!(state.pending_requests, 0);
            assert!(state.pending_inodes.is_empty());
        }
        f.dispatch.control.shutdown().unwrap();
    }

    #[test]
    fn idle_bulk_read_work_keeps_its_worker_and_inode_pin() {
        let f = fixture(false);
        let caller = std::thread::current().id();
        let (done, result) = mpsc::channel();
        f.dispatch.enqueue_on(
            true,
            false,
            CopyRequest::None,
            0,
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                let fs = state.unwrap();
                assert_ne!(std::thread::current().id(), caller);
                assert_eq!(fs.pending_inodes.get(&FUSE_ROOT_ID), Some(&1));
                assert_eq!(fs.pending_read_requests, 1);
                done.send(service(fs).metadata(Path::new("quick")).unwrap().len())
                    .unwrap();
            }),
        );
        assert_eq!(result.recv_timeout(Duration::from_secs(2)).unwrap(), 8);
        f.dispatch.control.shutdown().unwrap();
        let state = f.dispatch.state.lock().unwrap();
        assert_eq!(state.pending_requests, 0);
        assert!(state.pending_inodes.is_empty());
    }

    #[test]
    fn shutdown_waits_for_an_inline_callback_and_rejects_new_admission() {
        let f = fixture(false);
        let (entered, active) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let dispatch = f.dispatch.clone();
        let caller = std::thread::spawn(move || {
            dispatch.enqueue(
                CopyRequest::None,
                0,
                &[FUSE_ROOT_ID],
                Box::new(move |state| {
                    assert!(state.is_ok());
                    entered.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(5)).unwrap();
                }),
            );
        });
        active.recv_timeout(Duration::from_secs(2)).unwrap();
        let control = f.dispatch.control.clone();
        let (done, joined) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            control.shutdown().unwrap();
            done.send(()).unwrap();
        });
        while !f.dispatch.control.stopped.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(joined.try_recv().is_err());
        f.dispatch
            .metadata(|state| assert_eq!(state.err(), Some(libc::EIO)));
        resume.send(()).unwrap();
        caller.join().unwrap();
        joined.recv_timeout(Duration::from_secs(2)).unwrap();
        shutdown.join().unwrap();
        f.dispatch.enqueue_read(
            &[FUSE_ROOT_ID],
            Box::new(|state| {
                assert_eq!(state.err(), Some(libc::EIO));
            }),
        );
        assert_eq!(f.dispatch.state.lock().unwrap().pending_requests, 0);
    }

    #[test]
    fn metadata_completes_during_real_copy_preparation_and_mutations_stay_ordered() {
        let f = fixture(false);
        let (done, replies) = mpsc::channel();
        let first = done.clone();
        f.dispatch.enqueue(
            copy_request(),
            0,
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                let core = service(state.unwrap());
                let path = core.copy_up(Path::new("slow")).unwrap();
                fs::write(path, b"updated").unwrap();
                first.send(1).unwrap();
            }),
        );
        f.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        f.dispatch.enqueue(
            CopyRequest::None,
            0,
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                service(state.unwrap())
                    .rename(Path::new("slow"), Path::new("moved"), false)
                    .unwrap();
                done.send(2).unwrap();
            }),
        );
        let (fast, metadata) = mpsc::channel();
        let dispatch = f.dispatch.clone();
        let probe = std::thread::spawn(move || {
            dispatch.metadata(|state| {
                let core = service(state.unwrap());
                fast.send(core.metadata(Path::new("quick")).unwrap().len())
                    .unwrap();
            });
            dispatch.xattr_read(
                FUSE_ROOT_ID,
                Box::new(move |state| {
                    let backing = service(state.unwrap())
                        .observe_read_resolved(Path::new(""))
                        .unwrap();
                    assert!(backing.metadata.is_dir());
                    fast.send(0).unwrap();
                }),
            );
        });
        let result = metadata.recv_timeout(Duration::from_secs(1));
        let directory_observation = metadata.recv_timeout(Duration::from_secs(1));
        assert!(replies.try_recv().is_err());
        assert!(!f.root.path().join("upper/slow").exists());
        f.resume.send(()).unwrap();
        assert_eq!(result.unwrap(), 8);
        assert_eq!(directory_observation.unwrap(), 0);
        probe.join().unwrap();
        assert_eq!(replies.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(replies.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        f.dispatch.control.shutdown().unwrap();
        assert_eq!(
            fs::read(f.root.path().join("upper/moved")).unwrap(),
            b"updated"
        );
        assert_eq!(
            fs::read(f.root.path().join("lower/slow")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read_dir(f.root.path().join("upper")).unwrap().count(),
            2
        ); // moved + whiteout
    }

    #[test]
    fn admission_bounds_active_and_queued_requests_without_blocking_metadata() {
        let f = fixture(false);
        f.dispatch.enqueue(
            copy_request(),
            0,
            &[FUSE_ROOT_ID],
            Box::new(|state| {
                assert!(state.is_ok());
            }),
        );
        f.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, replies) = mpsc::channel();
        for _ in 1..REQUEST_LIMIT {
            let done = done.clone();
            f.dispatch.enqueue(
                CopyRequest::None,
                0,
                &[FUSE_ROOT_ID],
                Box::new(move |state| done.send(state.err()).unwrap()),
            );
        }
        let rejected = done.clone();
        f.dispatch.enqueue(
            CopyRequest::None,
            0,
            &[FUSE_ROOT_ID],
            Box::new(move |state| rejected.send(state.err()).unwrap()),
        );
        assert_eq!(
            replies.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(libc::EAGAIN)
        );
        // A full mutation lane cannot reject an unrelated open, backing read
        // or directory enumeration; exercise the shared service on that lane.
        let (sent, reads) = mpsc::channel();
        f.dispatch.enqueue_open(
            FUSE_ROOT_ID,
            libc::O_RDONLY,
            Box::new(move |state| {
                let core = service(state.unwrap());
                let backing = core
                    .prepare_open(Path::new("quick"), libc::O_RDONLY)
                    .unwrap();
                let file = fs::File::open(&backing).unwrap();
                sent.send((core, backing, file)).unwrap();
            }),
        );
        let (core, backing, file) = reads.recv_timeout(Duration::from_secs(2)).unwrap();
        let (sent, reads) = mpsc::channel();
        f.dispatch.enqueue_read(
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                assert!(state.is_ok());
                let mut bytes = [0; 8];
                assert_eq!(core.read_at(&backing, &file, &mut bytes, 0).unwrap(), 8);
                sent.send(bytes).unwrap();
            }),
        );
        assert_eq!(
            reads.recv_timeout(Duration::from_secs(2)).unwrap(),
            *b"metadata"
        );
        let (sent, reads) = mpsc::channel();
        f.dispatch.enqueue_read(
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                sent.send(
                    service(state.unwrap())
                        .directory_candidates(Path::new(""))
                        .unwrap(),
                )
                .unwrap();
            }),
        );
        assert!(
            reads
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .iter()
                .any(|(name, _)| name == "quick")
        );
        for flags in [libc::O_WRONLY, libc::O_RDWR, libc::O_TRUNC, libc::O_APPEND] {
            let (sent, reads) = mpsc::channel();
            f.dispatch.enqueue_open(
                FUSE_ROOT_ID,
                flags,
                Box::new(move |state| {
                    sent.send(state.err()).unwrap();
                }),
            );
            assert_eq!(
                reads.recv_timeout(Duration::from_secs(1)).unwrap(),
                Some(libc::EAGAIN)
            );
        }
        f.dispatch.metadata(|state| {
            assert_eq!(
                service(state.unwrap())
                    .metadata(Path::new("quick"))
                    .unwrap()
                    .len(),
                8
            )
        });
        f.resume.send(()).unwrap();
        f.dispatch.control.shutdown().unwrap();
        for _ in 1..REQUEST_LIMIT {
            assert_eq!(replies.recv().unwrap(), None);
        }
        assert_eq!(f.dispatch.state.lock().unwrap().pending_requests, 0);
        assert_eq!(f.dispatch.state.lock().unwrap().pending_read_requests, 0);
        assert!(f.dispatch.state.lock().unwrap().pending_inodes.is_empty());
    }

    #[test]
    fn shutdown_drains_the_read_worker_and_rejects_new_reads() {
        let f = fixture(false);
        // A real pending copy keeps reads on the independent worker. Keep the
        // existing drain test on that path, alongside the idle-inline drain test.
        f.dispatch.enqueue(
            copy_request(),
            0,
            &[FUSE_ROOT_ID],
            Box::new(|state| {
                assert!(state.is_ok());
            }),
        );
        f.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let (entered, active) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        f.dispatch.enqueue_read(
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                assert!(state.is_ok());
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
            }),
        );
        active.recv_timeout(Duration::from_secs(2)).unwrap();
        let control = f.dispatch.control.clone();
        let (done, joined) = mpsc::channel();
        let shutdown = std::thread::spawn(move || done.send(control.shutdown()).unwrap());
        while !f.dispatch.control.stopped.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let (rejected, response) = mpsc::channel();
        f.dispatch.enqueue_read(
            &[FUSE_ROOT_ID],
            Box::new(move |state| {
                rejected.send(state.err()).unwrap();
            }),
        );
        assert_eq!(
            response.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(libc::EIO)
        );
        let (rejected, response) = mpsc::channel();
        f.dispatch.enqueue_handle(
            FUSE_ROOT_ID,
            0,
            Box::new(move |state| rejected.send(state.err()).unwrap()),
        );
        assert_eq!(
            response.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(libc::EIO)
        );
        assert!(joined.try_recv().is_err());
        f.resume.send(()).unwrap();
        resume.send(()).unwrap();
        joined
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        shutdown.join().unwrap();
        let state = f.dispatch.state.lock().unwrap();
        assert_eq!(state.pending_requests, 0);
        assert_eq!(state.pending_read_requests, 0);
        assert!(state.pending_inodes.is_empty());
    }

    #[test]
    fn buffer_budget_failure_and_shutdown_preserve_request_completion() {
        let f = fixture(true);
        let (done, replies) = mpsc::channel();
        let first = done.clone();
        f.dispatch.enqueue(
            copy_request(),
            BYTE_LIMIT,
            &[FUSE_ROOT_ID],
            Box::new(move |state| first.send(state.err()).unwrap()),
        );
        f.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let reject = done.clone();
        f.dispatch.enqueue(
            CopyRequest::None,
            1,
            &[FUSE_ROOT_ID],
            Box::new(move |state| reject.send(state.err()).unwrap()),
        );
        assert_eq!(
            replies.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(libc::EAGAIN)
        );
        f.dispatch.enqueue(
            CopyRequest::None,
            0,
            &[FUSE_ROOT_ID],
            Box::new(move |state| done.send(state.err()).unwrap()),
        );
        let control = f.dispatch.control.clone();
        let (stopped, joined) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            control.shutdown().unwrap();
            stopped.send(()).unwrap();
        });
        while !f.dispatch.control.stopped.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(joined.try_recv().is_err());
        f.dispatch
            .metadata(|state| assert_eq!(state.err(), Some(libc::EIO)));
        f.resume.send(()).unwrap();
        joined.recv_timeout(Duration::from_secs(2)).unwrap();
        shutdown.join().unwrap();
        assert_eq!(replies.recv().unwrap(), Some(libc::ENOSPC));
        assert_eq!(replies.recv().unwrap(), None);
        assert_eq!(f.dispatch.state.lock().unwrap().pending_bytes, 0);
        assert_eq!(
            fs::read_dir(f.root.path().join("upper")).unwrap().count(),
            0
        );
        f.dispatch.control.shutdown().unwrap();
    }
}

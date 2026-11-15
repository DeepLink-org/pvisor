//! Default-off staging cache experiment. Credits count written bytes, not RSS.
use std::{
    fs::File,
    io,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
};

const BATCH: usize = 16 * 1024 * 1024;
const BUDGET: usize = 32 * 1024 * 1024;
pub(super) const ENV: &str = "PVISOR_EXPERIMENTAL_STAGING_CACHE";

pub(super) fn enabled(value: Option<&str>) -> io::Result<bool> {
    match value {
        None | Some("0") => Ok(false),
        Some("1") if cfg!(target_os = "linux") => Ok(true),
        Some("1") => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "staging cache experiment requires Linux",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staging cache experiment expects 0 or 1",
        )),
    }
}

pub(super) struct Cache {
    sender: Option<Sender<usize>>,
    replies: Receiver<io::Result<usize>>,
    worker: Option<JoinHandle<()>>,
    pending: usize,
    buffered: usize,
    failed: bool,
}

impl Cache {
    pub(super) fn start(file: File) -> io::Result<Self> {
        Self::spawn(move || {
            file.sync_data()?;
            advise(&file, 0, 0)
        })
    }

    fn spawn(mut flush: impl FnMut() -> io::Result<()> + Send + 'static) -> io::Result<Self> {
        let (sender, requests) = mpsc::channel::<usize>();
        let (results, replies) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ram-staging-cache".into())
            .spawn(move || {
                while let Ok(bytes) = requests.recv() {
                    let result = flush().map(|()| bytes);
                    let failed = result.is_err();
                    if results.send(result).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            replies,
            worker: Some(worker),
            pending: 0,
            buffered: 0,
            failed: false,
        })
    }

    fn check(&self) -> io::Result<()> {
        if self.failed {
            Err(io::Error::other("staging cache worker failed"))
        } else {
            Ok(())
        }
    }

    fn acknowledge(&mut self, result: io::Result<usize>) -> io::Result<()> {
        match result {
            Ok(bytes) => {
                self.pending -= bytes;
                Ok(())
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn wait(&mut self) -> io::Result<()> {
        let result = self
            .replies
            .recv()
            .unwrap_or_else(|_| Err(io::Error::other("staging cache worker disconnected")));
        self.acknowledge(result)
    }

    fn submit(&mut self) -> io::Result<()> {
        self.check()?;
        if self.buffered != 0 {
            if self.sender.as_ref().unwrap().send(self.buffered).is_err() {
                self.failed = true;
                return Err(io::Error::other("staging cache worker disconnected"));
            }
            self.pending += self.buffered;
            self.buffered = 0;
        }
        Ok(())
    }

    // The store has one serialized writer. Every staged write is <= 64 KiB.
    // Overwrites consume fresh credits; old acknowledgements cannot credit them.
    pub(super) fn before_write(&mut self, bytes: usize) -> io::Result<()> {
        self.check()?;
        if bytes > BUDGET {
            return Err(io::Error::other("staging write exceeds cache budget"));
        }
        loop {
            match self.replies.try_recv() {
                Ok(result) => self.acknowledge(result)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.failed = true;
                    return self.check();
                }
            }
        }
        if self.pending + self.buffered + bytes > BUDGET {
            self.submit()?;
            while self.pending + bytes > BUDGET {
                self.wait()?;
            }
        }
        Ok(())
    }

    pub(super) fn written(&mut self, bytes: usize) -> io::Result<()> {
        self.buffered += bytes;
        if self.buffered >= BATCH {
            self.submit()?;
        }
        Ok(())
    }

    // No worker may still reference the previous staging epoch at truncate.
    #[cfg(test)]
    pub(super) fn pending_bytes_for_test(&self) -> usize {
        self.pending + self.buffered
    }

    pub(super) fn drain(&mut self) -> io::Result<()> {
        self.submit()?;
        while self.pending != 0 {
            self.wait()?;
        }
        self.check()
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        if let Err(error) = self.drain() {
            tracing::warn!(%error, "staging cache worker drain failed");
        }
        self.sender.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("staging cache worker panicked");
        }
    }
}

// Advice never changes file contents or logical zero/dirty authority. Dirty or
// concurrently rewritten pages may remain cached; this is not a physical cap.
pub(super) fn advise(file: &File, offset: u64, length: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let offset = i64::try_from(offset)
            .map_err(|_| io::Error::other("staging advice offset overflow"))?;
        let length = i64::try_from(length)
            .map_err(|_| io::Error::other("staging advice length overflow"))?;
        let result = unsafe {
            libc::posix_fadvise(file.as_raw_fd(), offset, length, libc::POSIX_FADV_DONTNEED)
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, offset, length);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "staging cache experiment requires Linux",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_is_opt_in_and_strict() {
        assert!(!enabled(None).unwrap());
        assert!(!enabled(Some("0")).unwrap());
        assert!(enabled(Some("true")).is_err());
        assert!(enabled(Some("")).is_err());
        assert_eq!(enabled(Some("1")).is_ok(), cfg!(target_os = "linux"));
    }
    #[test]
    fn drop_flushes_partial_batch_and_joins() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = count.clone();
        let mut cache = Cache::spawn(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        cache.before_write(4096).unwrap();
        cache.written(4096).unwrap();
        drop(cache);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_worker_is_sticky_and_drains_without_hanging() {
        let mut cache = Cache::spawn(|| Err(io::Error::other("injected writeback error"))).unwrap();
        cache.before_write(4096).unwrap();
        cache.written(4096).unwrap();
        assert!(cache.drain().is_err());
        assert!(cache.before_write(4096).is_err());
    }
    #[test]
    fn credits_wait_for_completed_work_and_drop_joins() {
        let (entered, events) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let cache = Cache::spawn(move || {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(())
        })
        .unwrap();
        let (finished, done) = mpsc::channel();
        let producer = thread::spawn(move || {
            let mut cache = cache;
            cache.before_write(BATCH).unwrap();
            cache.written(BATCH).unwrap();
            cache.before_write(BATCH).unwrap();
            cache.written(BATCH).unwrap();
            cache.before_write(4096).unwrap();
            finished.send(()).unwrap();
            cache.written(4096).unwrap();
            cache.drain().unwrap();
        });
        events.recv().unwrap();
        assert!(done.try_recv().is_err());
        release.send(()).unwrap();
        done.recv().unwrap();
        events.recv().unwrap();
        release.send(()).unwrap();
        events.recv().unwrap();
        release.send(()).unwrap();
        producer.join().unwrap();
    }
}

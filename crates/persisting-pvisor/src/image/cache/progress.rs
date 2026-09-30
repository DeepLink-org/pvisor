//! Image size and per-run payload transfers, separate from guest network traffic.
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageTotals {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ImageProgress {
    pub image: String,
    pub totals: Option<ImageTotals>,
    pub downloaded_files: u64,
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub cached_files: u64,
    #[serde(default)]
    pub cached_bytes: u64,
}

static OUTPUT: OnceLock<Option<PathBuf>> = OnceLock::new();
pub(crate) fn init_output(path: Option<PathBuf>) {
    let _ = OUTPUT.set(path);
}

/// Keep blocking startup work visible without changing the cache protocol.
pub(super) fn loading<T>(
    label: &str,
    work: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    crate::diagnostics::diagnostic(format_args!("pVisor image: {label}"));
    let started = Instant::now();
    std::thread::scope(|scope| {
        let (done, wait) = mpsc::channel::<()>();
        scope.spawn(move || {
            while matches!(
                wait.recv_timeout(Duration::from_secs(5)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                crate::diagnostics::diagnostic(format_args!(
                    "pVisor image: {label}; still waiting ({:.0}s elapsed)",
                    started.elapsed().as_secs_f64()
                ));
            }
        });
        let result = work();
        drop(done);
        match &result {
            Ok(_) => crate::diagnostics::diagnostic(format_args!(
                "pVisor image: {label}; done ({:.1}s)",
                started.elapsed().as_secs_f64()
            )),
            Err(error) => crate::diagnostics::diagnostic(format_args!(
                "pVisor image: {label}; failed ({:.1}s): {error:#}",
                started.elapsed().as_secs_f64()
            )),
        }
        result
    })
}

#[derive(Default)]
pub(crate) struct Downloads {
    snapshot: RefCell<ImageProgress>,
    files: RefCell<HashSet<Vec<u8>>>,
    cached_files: RefCell<HashSet<Vec<u8>>>,
    output: Option<PathBuf>,
}
impl Downloads {
    pub fn new(image: &str) -> Self {
        let downloads = Self {
            snapshot: RefCell::new(ImageProgress {
                image: image.into(),
                ..Default::default()
            }),
            files: RefCell::new(HashSet::new()),
            cached_files: RefCell::new(HashSet::new()),
            output: OUTPUT.get().cloned().flatten(),
        };
        downloads.publish();
        downloads
    }
    pub fn totals(&self, totals: Option<ImageTotals>) {
        self.snapshot.borrow_mut().totals = totals;
        self.publish();
    }
    pub fn received(&self, path: &[u8], bytes: usize) {
        let first = self.files.borrow_mut().insert(path.to_vec());
        let mut snapshot = self.snapshot.borrow_mut();
        snapshot.downloaded_files += u64::from(first);
        snapshot.downloaded_bytes += bytes as u64;
        crate::diagnostics::diagnostic(format_args!(
            "pVisor image: transferred {bytes} bytes from /{} (this run: {} bytes across {} files)",
            String::from_utf8_lossy(path).escape_debug(),
            snapshot.downloaded_bytes,
            snapshot.downloaded_files,
        ));
        drop(snapshot);
        self.publish();
    }
    pub fn cached(&self, path: &[u8], bytes: usize) {
        let first = self.cached_files.borrow_mut().insert(path.to_vec());
        let mut snapshot = self.snapshot.borrow_mut();
        snapshot.cached_files += u64::from(first);
        snapshot.cached_bytes += bytes as u64;
        if first {
            crate::diagnostics::diagnostic(format_args!(
                "pVisor image: cached /{} ({bytes} bytes read; no download)",
                String::from_utf8_lossy(path).escape_debug(),
            ));
        }
        drop(snapshot);
        self.publish();
    }
    #[cfg(test)]
    pub fn snapshot(&self) -> ImageProgress {
        self.snapshot.borrow().clone()
    }

    fn publish(&self) {
        if let Some(path) = &self.output {
            // This transient UI snapshot needs atomic replacement, not durable fsync.
            let result = (|| -> anyhow::Result<()> {
                let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
                serde_json::to_writer(&mut file, &*self.snapshot.borrow())?;
                file.flush()?;
                file.persist(path)?;
                Ok(())
            })();
            if let Err(error) = result {
                tracing::debug!("image progress update: {error}");
            }
        }
    }
}

pub(super) fn image_totals(
    store: &crate::image::oci::ImageStore,
    digest: &str,
) -> anyhow::Result<ImageTotals> {
    let hex = crate::image::oci::digest_hex(digest)?;
    let record = store
        .root
        .join("metadata/sha256")
        .join(format!("{hex}.totals-v1.json"));
    if let Ok(bytes) = std::fs::read(&record)
        && let Ok(totals) = serde_json::from_slice(&bytes)
    {
        return Ok(totals);
    }
    let totals = scan_totals(&store.root.join("rootfs-v3/sha256").join(hex))?;
    crate::util::atomic_write(&record, &serde_json::to_vec(&totals)?, 0o600)?;
    Ok(totals)
}

fn scan_totals(root: &Path) -> anyhow::Result<ImageTotals> {
    let mut directories = vec![root.to_path_buf()];
    let mut totals = ImageTotals::default();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                directories.push(entry.path());
            } else if metadata.is_file() {
                totals.files += 1;
                totals.bytes = totals
                    .bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| anyhow::anyhow!("image size overflow"))?;
            }
        }
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loading_returns_the_result_and_preserves_error_context() {
        assert_eq!(loading("test success", || Ok(42)).unwrap(), 42);
        let error = loading::<()>("test failure", || {
            Err(anyhow::anyhow!("connection timed out").context("registry request"))
        })
        .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "registry request: connection timed out"
        );
    }

    #[test]
    fn totals_count_regular_paths_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("dir")).unwrap();
        std::fs::write(root.path().join("dir/file"), b"hello").unwrap();
        std::fs::write(root.path().join("empty"), b"").unwrap();
        std::os::unix::fs::symlink("dir", root.path().join("alias")).unwrap();
        std::os::unix::fs::symlink("/", root.path().join("outside")).unwrap();
        assert_eq!(
            scan_totals(root.path()).unwrap(),
            ImageTotals { files: 2, bytes: 5 }
        );
    }
    #[test]
    fn snapshots_count_each_file_once_but_all_transferred_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.json");
        let mut downloads = Downloads::new("example:latest");
        downloads.output = Some(path.clone());
        downloads.totals(Some(ImageTotals {
            files: 3,
            bytes: 100,
        }));
        downloads.received(b"a", 10);
        downloads.received(b"a", 10);
        downloads.received(b"b", 20);
        let snapshot: ImageProgress =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(snapshot.downloaded_files, 2);
        assert_eq!(snapshot.downloaded_bytes, 40);
        assert_eq!(snapshot.totals.unwrap().files, 3);
    }
}

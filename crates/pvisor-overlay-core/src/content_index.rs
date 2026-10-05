//! Digest-bound content receipts for an exclusively owned immutable baseline.
//! Import reuses existing digests. Lookup never opens the source file. This is
//! not a mutable-file cache: callers retain the baseline lease and prohibit all
//! external writers for the entire Core lifetime.
use crate::OverlayCore;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{self, Read},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};
const MAGIC: &[u8] = b"pvisor.base-content/1\n";
const WIDTH: usize = 64;
const LIMIT: usize = 64 * 1024 * 1024;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn decode(hex: &str) -> io::Result<[u8; 32]> {
    if hex.len() != 64 {
        return Err(invalid("invalid content digest"));
    }
    let mut bytes = [0; 32];
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let nibble = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(invalid("invalid content digest")),
        };
        bytes[i] = nibble(pair[0])? * 16 + nibble(pair[1])?;
    }
    Ok(bytes)
}
/// Encode all regular files from a verified import inventory. SHA-256 here
/// hashes relative path bytes, not file contents. Duplicate paths are rejected.
pub fn encode_content_index<'a>(
    files: impl IntoIterator<Item = (&'a Path, &'a str)>,
) -> io::Result<Vec<u8>> {
    let mut records = Vec::new();
    for (path, digest) in files {
        OverlayCore::validate_rel(path)?;
        let mut record = [0; WIDTH];
        record[..32].copy_from_slice(&Sha256::digest(path.as_os_str().as_bytes()));
        record[32..].copy_from_slice(&decode(digest)?);
        records.push(record);
        if records.len() > (LIMIT - MAGIC.len()) / WIDTH {
            return Err(invalid("content index exceeds limit"));
        }
    }
    records.sort_unstable();
    if records.windows(2).any(|r| r[0][..32] == r[1][..32]) {
        return Err(invalid("duplicate content index path"));
    }
    let mut bytes = Vec::with_capacity(MAGIC.len() + records.len() * WIDTH);
    bytes.extend_from_slice(MAGIC);
    for record in records {
        bytes.extend_from_slice(&record);
    }
    Ok(bytes)
}
#[derive(Debug)]
pub(crate) struct ContentIndex {
    path: PathBuf,
    digest: [u8; 32],
    records: Mutex<Option<Vec<u8>>>,
}
impl ContentIndex {
    pub fn new(path: PathBuf, digest: &str) -> io::Result<Self> {
        Ok(Self {
            path,
            digest: decode(digest)?,
            records: Mutex::new(None),
        })
    }
    #[cfg(test)]
    fn lookup(&self, path: &Path) -> io::Result<String> {
        self.lookup_present(path)?
            .ok_or_else(|| invalid("immutable baseline file missing from content index"))
    }

    pub fn lookup_with_metadata(
        &self,
        root: &Path,
        path: &Path,
        metadata: &std::fs::Metadata,
    ) -> io::Result<String> {
        use std::os::unix::fs::{DirEntryExt, MetadataExt};
        if let Some(digest) = self.lookup_present(path)? {
            return Ok(digest);
        }
        // Native case/Unicode aliases may resolve to a valid imported inode
        // whose name bytes differ from the request. Resolve only missing keys;
        // never hash source contents or substitute an unverified digest.
        let components = path.components().collect::<Vec<_>>();
        let mut actual = PathBuf::new();
        for (position, component) in components.iter().enumerate() {
            let std::path::Component::Normal(name) = component else {
                return Err(invalid("invalid content index path"));
            };
            let observed;
            let expected = if position + 1 == components.len() {
                metadata
            } else {
                observed = std::fs::symlink_metadata(root.join(&actual).join(name))?;
                &observed
            };
            let mut selected = None;
            for entry in std::fs::read_dir(root.join(&actual))? {
                let entry = entry?;
                if entry.ino() == expected.ino() {
                    let current = std::fs::symlink_metadata(entry.path())?;
                    if current.dev() == expected.dev() && current.ino() == expected.ino() {
                        selected = Some(entry.file_name());
                        break;
                    }
                }
            }
            actual.push(selected.ok_or_else(|| invalid("immutable baseline alias disappeared"))?);
        }
        self.lookup_present(&actual)?
            .ok_or_else(|| invalid("immutable baseline file missing from content index"))
    }

    fn lookup_present(&self, path: &Path) -> io::Result<Option<String>> {
        let mut cache = self
            .records
            .lock()
            .map_err(|_| io::Error::other("content index lock poisoned"))?;
        if cache.is_none() {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&self.path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > LIMIT as u64 {
                return Err(invalid("content index must be a regular file"));
            }
            let mut bytes = Vec::new();
            file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
            if bytes.len() > LIMIT
                || !bytes.starts_with(MAGIC)
                || !(bytes.len() - MAGIC.len()).is_multiple_of(WIDTH)
                || Sha256::digest(&bytes).as_slice() != self.digest
            {
                return Err(invalid("content index integrity mismatch"));
            }
            let records = bytes[MAGIC.len()..].as_chunks::<WIDTH>().0;
            if records.windows(2).any(|r| r[0][..32] >= r[1][..32]) {
                return Err(invalid("content index ordering mismatch"));
            }
            *cache = Some(bytes);
        }
        let key = Sha256::digest(path.as_os_str().as_bytes());
        let records = cache
            .as_ref()
            .ok_or_else(|| invalid("content index not loaded"))?;
        let records = records[MAGIC.len()..].as_chunks::<WIDTH>().0;
        let Ok(index) = records.binary_search_by(|r| r[..32].cmp(key.as_slice())) else {
            return Ok(None);
        };
        Ok(Some(super::core::hex_digest(&records[index][32..])))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_validate_digest_ordering_paths_and_missing_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index");
        let hash = "ab".repeat(32);
        let bytes = encode_content_index([(Path::new("file"), hash.as_str())]).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let digest = super::super::core::hex_digest(&Sha256::digest(&bytes));
        let index = ContentIndex::new(path.clone(), &digest).unwrap();
        assert_eq!(index.lookup(Path::new("file")).unwrap(), hash);
        assert!(index.lookup(Path::new("absent")).is_err());
        assert!(encode_content_index([(Path::new("../file"), hash.as_str())]).is_err());
        assert!(
            encode_content_index([
                (Path::new("file"), hash.as_str()),
                (Path::new("file"), hash.as_str())
            ])
            .is_err()
        );
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(
            ContentIndex::new(path, &digest)
                .unwrap()
                .lookup(Path::new("file"))
                .is_err()
        );
    }
    #[test]
    fn digest_matching_but_unsorted_and_symlink_indexes_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index");
        let hash = "ab".repeat(32);
        let mut bytes = encode_content_index([
            (Path::new("a"), hash.as_str()),
            (Path::new("b"), hash.as_str()),
        ])
        .unwrap();
        let offset = MAGIC.len();
        for i in 0..WIDTH {
            bytes.swap(offset + i, offset + WIDTH + i);
        }
        std::fs::write(&path, &bytes).unwrap();
        let digest = super::super::core::hex_digest(&Sha256::digest(&bytes));
        assert!(
            ContentIndex::new(path.clone(), &digest)
                .unwrap()
                .lookup(Path::new("a"))
                .is_err()
        );
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(path, &alias).unwrap();
        assert!(
            ContentIndex::new(alias, &digest)
                .unwrap()
                .lookup(Path::new("a"))
                .is_err()
        );
    }
    #[test]
    fn oversized_and_fifo_receipts_fail_without_reading_or_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized");
        std::fs::File::create(&path)
            .unwrap()
            .set_len((LIMIT + 1) as u64)
            .unwrap();
        let digest = "00".repeat(32);
        assert!(
            ContentIndex::new(path, &digest)
                .unwrap()
                .lookup(Path::new("a"))
                .is_err()
        );
        let fifo = temp.path().join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(
            ContentIndex::new(fifo, &digest)
                .unwrap()
                .lookup(Path::new("a"))
                .is_err()
        );
    }
}

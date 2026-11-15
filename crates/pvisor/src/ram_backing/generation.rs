//! A process-held flock protects a retired backing's conservative reservation.
//! Witnesses outlive connections and page handles: even a COW VMA can retain the
//! old file after its explicit page reference has gone away.
use super::invalid;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const MAX_RECORD_BYTES: u64 = 4096;
const MAX_RECORDS: usize = 256;
const MAX_PROCESS_GENERATIONS: usize = 32;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    generation: String,
    device: u64,
    inode: u64,
    length: u64,
    page_bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    generation: String,
}

fn private_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "generation directory must be private and same-user",
        ));
    }
    Ok(())
}

fn open_record(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > MAX_RECORD_BYTES
    {
        return Err(invalid("invalid generation record"));
    }
    Ok(file)
}

fn read_json<T: serde::de::DeserializeOwned>(file: &File) -> io::Result<T> {
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("generation record exceeds size limit"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn publish_json<T: Serialize>(path: &Path, value: &T, replace: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing record parent"))?;
    let mut pending = tempfile::Builder::new()
        .prefix(".pending-generation-")
        .tempfile_in(parent)?;
    pending
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(pending.as_file_mut(), value).map_err(io::Error::other)?;
    pending.flush()?;
    pending.as_file().sync_all()?;
    if replace {
        pending.persist(path).map_err(|e| e.error)?;
    } else {
        pending.persist_noclobber(path).map_err(|e| e.error)?;
    }
    File::open(parent)?.sync_all()
}

fn generation_id(value: &str) -> io::Result<[u8; 16]> {
    let id = uuid::Uuid::parse_str(value).map_err(io::Error::other)?;
    if id.is_nil() || id.to_string() != value {
        return Err(invalid("invalid backing generation identity"));
    }
    Ok(*id.as_bytes())
}

/// One published physical backing generation. The caller holds the pool's
/// exclusive service lock through publication and listener lifetime. Metadata
/// is immutable; the current pointer may change only for a fenced new owner.
pub struct PhysicalGeneration {
    directory: PathBuf,
    id: [u8; 16],
    name: String,
}

impl PhysicalGeneration {
    /// Publish an identity bound to the exact backing inode and native geometry.
    /// Requires a private pool directory and a fresh service-owned backing.
    /// No client can publish data before acquiring and registering a witness.
    pub fn publish(directory: &Path, backing: &File) -> io::Result<Self> {
        private_directory(directory)?;
        let records = directory.join("generations");
        match std::fs::DirBuilder::new().mode(0o700).create(&records) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => private_directory(&records)?,
            Err(e) => return Err(e),
        }
        for (count, entry) in std::fs::read_dir(&records)?.enumerate() {
            entry?;
            if count + 1 >= MAX_RECORDS {
                return Err(invalid("backing generation record budget exhausted"));
            }
        }
        let metadata = backing.metadata()?;
        let name = uuid::Uuid::new_v4().to_string();
        let record = Record {
            version: 1,
            generation: name.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            page_bytes: super::shared::PAGE,
        };
        validate_record(&record)?;
        publish_json(&records.join(format!("{name}.lease")), &record, false)?;
        publish_json(
            &directory.join("generation.json"),
            &Current {
                generation: name.clone(),
            },
            true,
        )?;
        Ok(Self {
            directory: directory.to_path_buf(),
            id: generation_id(&name)?,
            name,
        })
    }

    /// The opaque identity accepted by this owner's registration handshake.
    pub fn id(&self) -> [u8; 16] {
        self.id
    }

    /// Confirm a registered client has established a process-lifetime witness.
    /// Clients are trusted same-user host processes, as with mapping/release
    /// contracts. This check does not replace per-object byte verification.
    pub fn verify_registration(&self, id: [u8; 16]) -> io::Result<()> {
        if id != self.id {
            return Err(invalid(
                "registration belongs to another backing generation",
            ));
        }
        let file = open_record(
            &self
                .directory
                .join("generations")
                .join(format!("{}.lease", self.name)),
        )?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                FileExt::unlock(&file)?;
                Err(invalid("generation has no process-lifetime witness"))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Sum conservative full-file reservations for witnessed retired generations.
    /// Unwitnessed records can be removed only while holding their exclusive lock.
    /// Caller must not hold the content-pool lock during filesystem operations.
    pub fn retained_bytes(&self) -> io::Result<usize> {
        retained_bytes(&self.directory, Some(self.id))
    }

    /// Verify the saved current record before a manager may replace a dead
    /// physical service's endpoint. Does not prove owner death: the caller must
    /// also acquire the exclusive service lock and validate the socket inode.
    pub fn recoverable(directory: &Path) -> io::Result<()> {
        private_directory(directory)?;
        let current: Current = read_json(&open_record(&directory.join("generation.json"))?)?;
        generation_id(&current.generation)?;
        let records = directory.join("generations");
        private_directory(&records)?;
        let record: Record = read_json(&open_record(
            &records.join(format!("{}.lease", current.generation)),
        )?)?;
        validate_record(&record)?;
        if current.generation != record.generation {
            return Err(invalid("current generation record mismatch"));
        }
        Ok(())
    }
}

fn validate_record(record: &Record) -> io::Result<()> {
    generation_id(&record.generation)?;
    if record.version != 1
        || record.page_bytes != super::shared::PAGE
        || record.length == 0
        || record.length > isize::MAX as u64
        || !record.length.is_multiple_of(record.page_bytes as u64)
    {
        return Err(invalid("invalid generation geometry"));
    }
    Ok(())
}

/// Conservative retired reservations before publishing a fresh generation.
/// Must be called only after fencing the old owner with the service lock.
pub fn retired_bytes_before_start(directory: &Path) -> io::Result<usize> {
    retained_bytes(directory, None)
}

fn retained_bytes(directory: &Path, active: Option<[u8; 16]>) -> io::Result<usize> {
    let records = directory.join("generations");
    if !records.try_exists()? {
        return Ok(0);
    }
    private_directory(&records)?;
    // Keep the current pointer resolvable across a crash between retirement
    // inspection and publication of its replacement.
    let current_path = directory.join("generation.json");
    let current_id = if current_path.try_exists()? {
        let current: Current = read_json(&open_record(&current_path)?)?;
        Some(generation_id(&current.generation)?)
    } else {
        None
    };
    let mut total = 0usize;
    for (count, entry) in std::fs::read_dir(&records)?.enumerate() {
        if count >= MAX_RECORDS {
            return Err(invalid("backing generation record budget exhausted"));
        }
        let entry = entry?;
        let path = entry.path();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".pending-generation-")
        {
            // Only startup publishes metadata, under the service lock. A
            // private pending file seen here is from an interrupted old owner.
            let _checked = open_record(&path)?;
            std::fs::remove_file(path)?;
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "lease") {
            return Err(invalid("unexpected backing generation directory entry"));
        }
        let file = open_record(&path)?;
        let record: Record = read_json(&file)?;
        validate_record(&record)?;
        if path.file_stem().and_then(|s| s.to_str()) != Some(record.generation.as_str()) {
            return Err(invalid("backing generation filename mismatch"));
        }
        if Some(generation_id(&record.generation)?) == active {
            continue;
        }
        match file.try_lock_exclusive() {
            Ok(()) => {
                let opened = file.metadata()?;
                let current = std::fs::symlink_metadata(&path)?;
                if opened.dev() != current.dev() || opened.ino() != current.ino() {
                    return Err(invalid("generation changed during retirement"));
                }
                if Some(generation_id(&record.generation)?) != current_id {
                    std::fs::remove_file(path)?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                total = total
                    .checked_add(record.length as usize)
                    .ok_or_else(|| invalid("retired generation byte overflow"))?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// A shared witness lock acquired before any object publication. Retaining it
/// for process lifetime also covers COW VMAs and read-only verification aliases
/// that can outlive explicit page references. It is never a TTL lease.
pub struct GenerationLease {
    id: [u8; 16],
    file: File,
}

impl GenerationLease {
    /// Validate the private current record against the exact received backing
    /// before registering with the publisher. A racing owner replacement fails
    /// without modifying existing references. Uses nonblocking flock.
    pub fn acquire(directory: &Path, backing: &File) -> io::Result<Self> {
        private_directory(directory)?;
        let current: Current = read_json(&open_record(&directory.join("generation.json"))?)?;
        let id = generation_id(&current.generation)?;
        let records = directory.join("generations");
        private_directory(&records)?;
        let file = open_record(&records.join(format!("{}.lease", current.generation)))?;
        FileExt::try_lock_shared(&file)?;
        let record: Record = read_json(&file)?;
        validate_record(&record)?;
        let metadata = backing.metadata()?;
        if record.generation != current.generation
            || record.device != metadata.dev()
            || record.inode != metadata.ino()
            || record.length != metadata.len()
        {
            return Err(invalid("generation record does not match received backing"));
        }
        Ok(Self { id, file })
    }

    /// Identity sent to this generation's registration handshake.
    pub fn id(&self) -> [u8; 16] {
        self.id
    }

    /// Keep the witness until process exit. No mapped backing may be inherited
    /// by another process. The bounded registry does not release witnesses when
    /// the daemon connection, store or individual page handles are dropped.
    pub fn retain_for_process(self) -> io::Result<()> {
        static WITNESSES: OnceLock<Mutex<BTreeMap<(u64, u64), File>>> = OnceLock::new();
        let metadata = self.file.metadata()?;
        let key = (metadata.dev(), metadata.ino());
        let mut witnesses = WITNESSES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| io::Error::other("generation witness registry poisoned"))?;
        if witnesses.contains_key(&key) {
            return Ok(());
        }
        if witnesses.len() >= MAX_PROCESS_GENERATIONS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process backing generation witness budget exhausted",
            ));
        }
        witnesses.insert(key, self.file);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ram_backing::shared::{PAGE, SharedPool};

    fn directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn pool() -> SharedPool {
        SharedPool::new(PAGE, 1).unwrap()
    }

    #[test]
    fn registration_requires_the_exact_backing_and_a_live_witness() {
        let directory = directory();
        let pool = pool();
        let generation =
            PhysicalGeneration::publish(directory.path(), &pool.readonly_file()).unwrap();
        PhysicalGeneration::recoverable(directory.path()).unwrap();
        assert!(generation.verify_registration(generation.id()).is_err());
        let other = self::pool();
        assert!(GenerationLease::acquire(directory.path(), &other.readonly_file()).is_err());
        let lease = GenerationLease::acquire(directory.path(), &pool.readonly_file()).unwrap();
        assert_eq!(lease.id(), generation.id());
        generation.verify_registration(lease.id()).unwrap();
        assert!(generation.verify_registration([0; 16]).is_err());
        drop(lease);
        assert!(generation.verify_registration(generation.id()).is_err());
    }

    #[test]
    fn retirement_counts_a_generation_once_and_never_deletes_the_current_pointer() {
        let directory = directory();
        let first = pool();
        let a = PhysicalGeneration::publish(directory.path(), &first.readonly_file()).unwrap();
        let lease = GenerationLease::acquire(directory.path(), &first.readonly_file()).unwrap();
        let alias = GenerationLease::acquire(directory.path(), &first.readonly_file()).unwrap();
        assert_eq!(
            a.retained_bytes().unwrap(),
            0,
            "active backing is charged separately"
        );
        assert_eq!(retired_bytes_before_start(directory.path()).unwrap(), PAGE);
        let second = pool();
        let b = PhysicalGeneration::publish(directory.path(), &second.readonly_file()).unwrap();
        assert_eq!(b.retained_bytes().unwrap(), PAGE);
        drop(lease);
        assert_eq!(b.retained_bytes().unwrap(), PAGE);
        drop(alias);
        assert_eq!(b.retained_bytes().unwrap(), 0);
        assert!(
            !directory
                .path()
                .join("generations")
                .join(format!("{}.lease", a.name))
                .exists()
        );
        assert_eq!(retired_bytes_before_start(directory.path()).unwrap(), 0);
        PhysicalGeneration::recoverable(directory.path()).unwrap();
        assert!(
            directory
                .path()
                .join("generations")
                .join(format!("{}.lease", b.name))
                .exists()
        );
    }

    #[test]
    fn process_witness_outlives_the_negotiation_and_explicit_reference_scope() {
        let directory = directory();
        let first = pool();
        let a = PhysicalGeneration::publish(directory.path(), &first.readonly_file()).unwrap();
        GenerationLease::acquire(directory.path(), &first.readonly_file())
            .unwrap()
            .retain_for_process()
            .unwrap();
        a.verify_registration(a.id()).unwrap();
        drop(first);
        let second = pool();
        let b = PhysicalGeneration::publish(directory.path(), &second.readonly_file()).unwrap();
        assert_eq!(b.retained_bytes().unwrap(), PAGE);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn witness_child() {
        let Some(directory) = std::env::var_os("PVISOR_GENERATION_TEST_DIRECTORY") else {
            return;
        };
        let backing =
            File::open(std::env::var_os("PVISOR_GENERATION_TEST_BACKING").unwrap()).unwrap();
        GenerationLease::acquire(Path::new(&directory), &backing)
            .unwrap()
            .retain_for_process()
            .unwrap();
        println!("witness-ready");
        std::io::stdout().flush().unwrap();
        // Exit only after the parent has observed the retired reservation.
        std::io::stdin().read_exact(&mut [0]).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn retired_reservation_is_reclaimed_only_after_the_witness_process_exits() {
        use std::{
            io::BufRead,
            os::fd::AsRawFd,
            process::{Child, Command, Stdio},
        };
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let directory = directory();
        let first = pool();
        PhysicalGeneration::publish(directory.path(), &first.readonly_file()).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ram_backing::generation::tests::witness_child",
                    "--nocapture",
                ])
                .env("PVISOR_GENERATION_TEST_DIRECTORY", directory.path())
                .env(
                    "PVISOR_GENERATION_TEST_BACKING",
                    format!(
                        "/proc/{}/fd/{}",
                        std::process::id(),
                        first.readonly_file().as_raw_fd()
                    ),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = std::io::BufReader::new(child.0.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "witness child exited before readiness"
            );
            if line.trim() == "witness-ready" {
                break;
            }
        }
        let second = pool();
        let generation =
            PhysicalGeneration::publish(directory.path(), &second.readonly_file()).unwrap();
        assert_eq!(generation.retained_bytes().unwrap(), PAGE);
        child.0.stdin.take().unwrap().write_all(&[1]).unwrap();
        assert!(child.0.wait().unwrap().success());
        assert_eq!(generation.retained_bytes().unwrap(), 0);
    }

    #[test]
    fn unsafe_or_corrupt_metadata_fails_closed_and_orphan_records_are_collected() {
        let directory = directory();
        let pool = pool();
        let generation =
            PhysicalGeneration::publish(directory.path(), &pool.readonly_file()).unwrap();
        let path = directory
            .path()
            .join("generations")
            .join(format!("{}.lease", generation.name));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(GenerationLease::acquire(directory.path(), &pool.readonly_file()).is_err());
        assert!(retired_bytes_before_start(directory.path()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"{broken").unwrap();
        assert!(PhysicalGeneration::recoverable(directory.path()).is_err());
        std::fs::write(&path, original).unwrap();
        let orphan = directory
            .path()
            .join("generations/.pending-generation-orphan");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&orphan)
            .unwrap();
        assert_eq!(retired_bytes_before_start(directory.path()).unwrap(), 0);
        assert!(!orphan.exists());
        // A crash before replacement publication must leave current resolvable.
        PhysicalGeneration::recoverable(directory.path()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(directory.path().join("generation.json"), &path).unwrap();
        assert!(PhysicalGeneration::recoverable(directory.path()).is_err());
    }
}

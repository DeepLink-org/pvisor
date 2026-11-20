//! Linux snapshot mount identity and ordinary retirement.
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::{self, File},
    io,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Inode {
    device: u64,
    inode: u64,
}

impl Inode {
    pub(super) fn directory(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Err(io::Error::other("snapshot mountpoint is not a directory"));
        }
        Ok(Self::from_metadata(&metadata))
    }

    pub(super) fn file(file: &File) -> io::Result<Self> {
        Ok(Self::from_metadata(&file.metadata()?))
    }

    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Identity {
    namespace: Inode,
    id: u64,
    device: Vec<u8>,
}

struct MountTable {
    text: Vec<u8>,
    path: Vec<u8>,
    source: String,
}

pub(super) fn source(path: &Path) -> String {
    format!(
        "pvisor-snapshot-ram-{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

impl MountTable {
    fn read(path: &Path) -> io::Result<Self> {
        let mut encoded = Vec::new();
        for byte in path.as_os_str().as_bytes() {
            match byte {
                b' ' => encoded.extend_from_slice(b"\\040"),
                b'\t' => encoded.extend_from_slice(b"\\011"),
                b'\n' => encoded.extend_from_slice(b"\\012"),
                b'\\' => encoded.extend_from_slice(b"\\134"),
                byte => encoded.push(*byte),
            }
        }
        Ok(Self {
            text: fs::read("/proc/thread-self/mountinfo")?,
            path: encoded,
            source: source(path),
        })
    }

    fn at_path(&self, namespace: Inode) -> io::Result<Option<Identity>> {
        let mut found = None;
        for line in self
            .text
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let mut fields = line.split(|byte| *byte == b' ');
            let id = fields.next();
            fields.next(); // parent mount ID
            let device = fields.next();
            let root = fields.next();
            if fields.next() != Some(self.path.as_slice()) {
                continue;
            }
            if found.is_some()
                || root != Some(b"/")
                || fields.find(|field| *field == b"-").is_none()
                || fields.next() != Some(b"fuse")
                || fields.next() != Some(self.source.as_bytes())
            {
                return Err(io::Error::other("unexpected or stacked snapshot RAM mount"));
            }
            let id = id
                .and_then(|id| std::str::from_utf8(id).ok())
                .and_then(|id| id.parse().ok())
                .ok_or_else(|| io::Error::other("invalid snapshot RAM mount ID"))?;
            let device = device
                .filter(|device| !device.is_empty())
                .ok_or_else(|| io::Error::other("missing snapshot RAM mount device"))?;
            found = Some(Identity {
                namespace,
                id,
                device: device.to_vec(),
            });
        }
        Ok(found)
    }

    fn contains(&self, id: u64) -> bool {
        let id = id.to_string();
        self.text
            .split(|byte| *byte == b'\n')
            .any(|line| line.split(|byte| *byte == b' ').next() == Some(id.as_bytes()))
    }
}

fn mount_namespace() -> io::Result<Inode> {
    Ok(Inode::from_metadata(&fs::metadata(
        "/proc/thread-self/ns/mnt",
    )?))
}

impl Identity {
    pub(super) fn capture(path: &Path) -> io::Result<Self> {
        MountTable::read(path)?
            .at_path(mount_namespace()?)?
            .ok_or_else(|| io::Error::other("snapshot RAM mount missing from mount table"))
    }

    pub(super) fn verify_file(&self, file: &File) -> io::Result<()> {
        let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
        let id = info
            .lines()
            .find_map(|line| line.strip_prefix("mnt_id:")?.trim().parse::<u64>().ok());
        if id != Some(self.id) || mount_namespace()? != self.namespace {
            return Err(io::Error::other(
                "snapshot RAM file belongs to a different mount",
            ));
        }
        Ok(())
    }
}

fn with_identity(
    table: &MountTable,
    namespace: Inode,
    expected: Option<&Identity>,
    unmount: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if expected.is_some_and(|expected| expected.namespace != namespace) {
        return Err(io::Error::other("snapshot RAM mount namespace changed"));
    }
    match (table.at_path(namespace)?, expected) {
        (Some(current), Some(expected)) if &current == expected => unmount(),
        (None, Some(expected)) if table.contains(expected.id) => {
            Err(io::Error::other("snapshot RAM mount moved"))
        }
        (None, _) => Ok(()),
        _ => Err(io::Error::other("snapshot RAM mount identity changed")),
    }
}

fn ordinary_unmount(path: &Path) -> io::Result<()> {
    let native = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: native is NUL-terminated. NOFOLLOW protects the final pathname
    // component without changing ordinary unmount's busy-reference checks.
    if unsafe { libc::umount2(native.as_ptr(), libc::UMOUNT_NOFOLLOW) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EPERM) {
        return Err(error);
    }
    for helper in ["fusermount3", "fusermount"] {
        let mut child = match Command::new(helper)
            .args(["-u", "--"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let status = super::wait_for_exit(&mut child, Duration::from_secs(2))?;
        return if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{helper} exited with {status}")))
        };
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "FUSE unmount helper unavailable",
    ))
}

pub(super) fn retire(path: &Path, directory: Inode, expected: Option<&Identity>) -> io::Result<()> {
    let namespace = mount_namespace()?;
    retire_with(
        path,
        directory,
        expected,
        namespace,
        Instant::now() + Duration::from_secs(3),
        || MountTable::read(path),
        || ordinary_unmount(path),
    )
}

fn retire_with(
    path: &Path,
    directory: Inode,
    expected: Option<&Identity>,
    namespace: Inode,
    deadline: Instant,
    mut read: impl FnMut() -> io::Result<MountTable>,
    mut unmount: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    loop {
        let table = read()?;
        let mut attempted = false;
        match with_identity(&table, namespace, expected, || {
            attempted = true;
            unmount()
        }) {
            // The unprivileged helper has a generic failure exit code. Retry
            // while the same mount is still present, and let the kernel drain.
            Err(error)
                if attempted
                    && Instant::now() < deadline
                    && (error.raw_os_error() == Some(libc::EBUSY)
                        || error.kind() == io::ErrorKind::Other) =>
            {
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            result => result?,
        }
        break;
    }
    let table = read()?;
    with_identity(&table, namespace, expected, || {
        Err(io::Error::other(
            "snapshot RAM mount still present after unmount",
        ))
    })?;
    match Inode::directory(path) {
        Ok(identity) if identity == directory => fs::remove_dir(path),
        Ok(_) => Err(io::Error::other("snapshot RAM mountpoint inode changed")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn table(line: &str) -> MountTable {
        MountTable {
            text: line.as_bytes().to_vec(),
            path: b"/tmp/ram-mount-\\040fixture".to_vec(),
            source: "pvisor-snapshot-ram-fixture".to_owned(),
        }
    }

    const EXPECTED: &str =
        "45 30 0:67 / /tmp/ram-mount-\\040fixture ro - fuse pvisor-snapshot-ram-fixture ro\n";
    const REPLACED: &str =
        "46 30 0:68 / /tmp/ram-mount-\\040fixture ro - fuse pvisor-snapshot-ram-fixture ro\n";
    const MOVED: &str = "45 30 0:67 / /other ro - fuse pvisor-snapshot-ram-fixture ro\n";
    const FOREIGN: &str = "46 30 0:68 / /tmp/ram-mount-\\040fixture rw - tmpfs tmpfs rw\n";

    #[test]
    fn ordinary_unmount_must_remove_the_expected_id_before_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram-mount- fixture");
        fs::create_dir(&path).unwrap();
        let inode = Inode::directory(&path).unwrap();
        let ns = mount_namespace().unwrap();
        let identity = table(EXPECTED).at_path(ns).unwrap().unwrap();
        let mounted = Cell::new(true);
        let result = retire_with(
            &path,
            inode,
            Some(&identity),
            ns,
            Instant::now(),
            || Ok(table(if mounted.get() { EXPECTED } else { "" })),
            || {
                assert!(mounted.replace(false));
                assert!(path.is_dir());
                Ok(())
            },
        );
        result.unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn busy_or_unverified_success_retains_the_original_directory() {
        let dir = tempfile::tempdir().unwrap();
        let inode = Inode::directory(dir.path()).unwrap();
        let ns = mount_namespace().unwrap();
        let identity = table(EXPECTED).at_path(ns).unwrap().unwrap();
        for raw_error in [Some(libc::EBUSY), Some(libc::EPERM), None] {
            let calls = Cell::new(0);
            let result = retire_with(
                dir.path(),
                inode,
                Some(&identity),
                ns,
                Instant::now(),
                || Ok(table(EXPECTED)),
                || {
                    calls.set(calls.get() + 1);
                    raw_error.map_or(Ok(()), |e| Err(io::Error::from_raw_os_error(e)))
                },
            );
            assert!(result.is_err());
            assert_eq!(calls.get(), 1);
            assert_eq!(Inode::directory(dir.path()).unwrap(), inode);
        }
    }

    #[test]
    fn replacement_moved_stacked_and_wrong_namespace_never_invoke_unmount() {
        let ns = mount_namespace().unwrap();
        let identity = table(EXPECTED).at_path(ns).unwrap().unwrap();
        for lines in [REPLACED, MOVED, FOREIGN, &format!("{EXPECTED}{FOREIGN}")] {
            assert!(
                with_identity(&table(lines), ns, Some(&identity), || panic!(
                    "unexpected unmount"
                ))
                .is_err()
            );
        }
        let wrong_ns = Inode {
            inode: ns.inode.wrapping_add(1),
            ..ns
        };
        assert!(
            with_identity(&table(EXPECTED), wrong_ns, Some(&identity), || panic!(
                "wrong namespace unmount"
            ))
            .is_err()
        );
    }

    #[test]
    fn already_gone_mount_can_remove_only_its_unchanged_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        fs::create_dir(&original).unwrap();
        let inode = Inode::directory(&original).unwrap();
        let ns = mount_namespace().unwrap();
        let expected = table(EXPECTED).at_path(ns).unwrap().unwrap();
        let replacement = dir.path().join("replacement");
        fs::create_dir(&replacement).unwrap();
        assert!(
            retire_with(
                &replacement,
                inode,
                Some(&expected),
                ns,
                Instant::now(),
                || Ok(table("")),
                || panic!("unexpected unmount")
            )
            .is_err()
        );
        let marker = original.join("retain");
        fs::write(&marker, b"not owned by the pager").unwrap();
        assert!(
            retire_with(
                &original,
                inode,
                Some(&expected),
                ns,
                Instant::now(),
                || Ok(table("")),
                || panic!("unexpected unmount")
            )
            .is_err()
        );
        assert!(marker.exists());
        assert!(replacement.exists());
    }
}

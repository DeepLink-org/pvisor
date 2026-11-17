use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Seek};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum RamIdentity {
    // Existing version-2 manifests store the full-file digest as a string.
    Full(String),
    Verity { fsverity_sha256: [u8; 32] },
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

impl RamIdentity {
    pub(super) fn publish(path: &Path, captured: File) -> anyhow::Result<Self> {
        let file = open(path)?;
        let original = captured.metadata()?;
        let reopened = file.metadata()?;
        ensure!(
            (original.dev(), original.ino()) == (reopened.dev(), reopened.ino()),
            "branch RAM replaced before publication"
        );
        drop(captured); // Enabling verity requires all writable descriptors closed.
        #[cfg(target_os = "linux")]
        if verity::enable(&file)? {
            return Ok(Self::Verity {
                fsverity_sha256: verity::measure(&file)?,
            });
        }
        Ok(Self::Full(crate::environment_snapshot::hash_open_file(
            &file,
        )?))
    }

    pub(super) fn phase(&self) -> &'static str {
        match self {
            Self::Full(_) => "ram-hash",
            Self::Verity { .. } => "ram-verity",
        }
    }

    pub(super) fn open_verified(&self, path: &Path) -> anyhow::Result<File> {
        let mut file = open(path)?;
        match self {
            Self::Full(expected) => {
                ensure!(
                    crate::environment_snapshot::hash_open_file(&file)? == *expected,
                    "branch RAM content changed"
                );
                file.rewind()?;
            }
            #[cfg(target_os = "linux")]
            Self::Verity { fsverity_sha256 } => {
                ensure!(
                    verity::measure(&file)? == *fsverity_sha256,
                    "branch RAM verity digest changed"
                );
            }
            #[cfg(not(target_os = "linux"))]
            Self::Verity { .. } => anyhow::bail!("branch RAM requires Linux fs-verity"),
        }
        Ok(file)
    }
}

#[cfg(target_os = "linux")]
mod verity {
    use super::*;
    use std::os::fd::AsRawFd;

    // Linux UAPI <linux/fsverity.h>. All optional fields are zero.
    #[repr(C)]
    struct Enable {
        version: u32,
        hash_algorithm: u32,
        block_size: u32,
        salt_size: u32,
        salt_ptr: u64,
        sig_size: u32,
        reserved1: u32,
        sig_ptr: u64,
        reserved2: [u64; 11],
    }

    pub(super) fn enable(file: &File) -> io::Result<bool> {
        let arg = Enable {
            version: 1,
            hash_algorithm: 1,
            block_size: 4096,
            salt_size: 0,
            salt_ptr: 0,
            sig_size: 0,
            reserved1: 0,
            sig_ptr: 0,
            reserved2: [0; 11],
        };
        let request = libc::_IOW::<Enable>(b'f' as u32, 133);
        if unsafe { libc::ioctl(file.as_raw_fd(), request, &arg) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOTTY | libc::EOPNOTSUPP | libc::ENOPKG | libc::EPERM) => Ok(false),
            _ => Err(error),
        }
    }

    #[repr(C)]
    struct Digest {
        algorithm: u16,
        size: u16,
        bytes: [u8; 32],
    }

    pub(super) fn measure(file: &File) -> io::Result<[u8; 32]> {
        let mut digest = Digest {
            algorithm: 0,
            size: 32,
            bytes: [0; 32],
        };
        // The ioctl number encodes only the fixed four-byte header.
        let request = libc::_IOWR::<[u16; 2]>(b'f' as u32, 134);
        if unsafe { libc::ioctl(file.as_raw_fd(), request, &mut digest) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if digest.algorithm != 1 || digest.size != 32 {
            return Err(io::Error::other("unexpected fs-verity digest format"));
        }
        Ok(digest.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read};

    #[test]
    fn branch_ram_verification_binds_the_mapped_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram");
        fs::write(&path, b"original").unwrap();
        let identity =
            RamIdentity::publish(&path, OpenOptions::new().write(true).open(&path).unwrap())
                .unwrap();
        let mut verified = identity.open_verified(&path).unwrap();
        fs::rename(&path, dir.path().join("old")).unwrap();
        fs::write(&path, b"replaced").unwrap();
        assert!(identity.open_verified(&path).is_err());
        let mut contents = String::new();
        verified.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "original");
    }

    #[test]
    fn verity_identity_never_falls_back_to_an_unsealed_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let identity = RamIdentity::Verity {
            fsverity_sha256: [0; 32],
        };
        assert!(identity.open_verified(file.path()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires TMPDIR on an fs-verity-enabled filesystem"]
    fn sealed_branch_ram_rejects_writes_and_another_sealed_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram");
        fs::write(&path, b"original").unwrap();
        let identity =
            RamIdentity::publish(&path, OpenOptions::new().write(true).open(&path).unwrap())
                .unwrap();
        assert!(matches!(identity, RamIdentity::Verity { .. }));
        identity.open_verified(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        fs::rename(&path, dir.path().join("old")).unwrap();
        fs::write(&path, b"replaced").unwrap();
        RamIdentity::publish(&path, OpenOptions::new().write(true).open(&path).unwrap()).unwrap();
        assert!(identity.open_verified(&path).is_err());
    }
}

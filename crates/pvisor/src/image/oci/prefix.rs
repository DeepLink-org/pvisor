//! Reuse a verified, immutable OCI layer prefix within one private store and host boot.

use super::{Descriptor, digest_hex};
use crate::image::store::{LocalStore, private_directory, remove_import};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const CACHE: &str = "oci-prefix-v1";
const CUT_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) struct Prefix {
    pub(crate) root: PathBuf,
    pub(crate) key: String,
    pub(crate) len: usize,
    pub(crate) reused: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    key: String,
    layers: usize,
    device: u64,
    inode: u64,
}

// Do not cache the last layer. A complete image already has a separate publication path.
fn cut(layers: &[Descriptor], bytes: u64) -> Option<usize> {
    let (_, candidates) = layers.split_last()?;
    let mut total = 0u64;
    for (index, layer) in candidates.iter().enumerate() {
        total = total.saturating_add(layer.size?);
        if total >= bytes {
            return Some(index + 1);
        }
    }
    None
}

fn positive_decimal(setting: &OsStr, error: &'static str) -> Result<u64> {
    let text = setting.to_str().context(error)?;
    ensure!(
        !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit()),
        "{error}"
    );
    text.parse().context(error)
}

fn selected_depth(
    layers: &[Descriptor],
    depth_setting: Option<&OsStr>,
    suffix_limit: Option<&OsStr>,
) -> Result<Option<usize>> {
    let depth = if let Some(depth_setting) = depth_setting {
        let error = "PVISOR_OCI_PREFIX_LAYERS must be a positive decimal layer count";
        let depth = usize::try_from(positive_decimal(depth_setting, error)?).context(error)?;
        (depth < layers.len()).then_some(depth)
    } else {
        cut(layers, CUT_BYTES)
    };
    let Some(limit) = suffix_limit else {
        return Ok(depth);
    };
    let limit = positive_decimal(
        limit,
        "PVISOR_OCI_PREFIX_MAX_SUFFIX_LAYER_BYTES must be a positive decimal byte count",
    )?;
    Ok(depth.filter(|&depth| {
        !layers[depth..]
            .iter()
            .filter_map(|layer| layer.size)
            .any(|bytes| bytes >= limit)
    }))
}

fn chain_key(platform: &str, layers: &[Descriptor]) -> Result<String> {
    let descriptors: Vec<_> = layers
        .iter()
        .map(|d| {
            digest_hex(&d.digest)?;
            Ok((&d.digest, &d.media_type, d.size))
        })
        .collect::<Result<_>>()?;
    let identity = serde_json::to_vec(&(CACHE, platform, descriptors))?;
    Ok(crate::util::encode_hex(&Sha256::digest(identity)))
}

fn boot() -> Result<uuid::Uuid> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .parse()?)
}

fn owned_directory(path: &Path) -> Result<()> {
    let stat = fs::symlink_metadata(path)?;
    ensure!(
        stat.is_dir() && stat.uid() == unsafe { libc::geteuid() } && stat.mode() & 0o077 == 0,
        "unexpected OCI prefix directory: {}",
        path.display()
    );
    Ok(())
}

fn current_epoch(store: &LocalStore, boot: uuid::Uuid) -> Result<PathBuf> {
    let cache = store.root.join(CACHE);
    private_directory(&cache)?;
    let epoch = cache.join(boot.to_string());
    match fs::symlink_metadata(&epoch) {
        Ok(_) => {
            owned_directory(&epoch)?;
            return Ok(epoch);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let _setup = store.lock_file("oci-prefix-epoch-v1")?;
    // A reboot invalidates unflushed extracted files and all live local mounts.
    // Retire old epochs before creating the new epoch so interruption can be retried.
    for entry in fs::read_dir(&cache)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.path() != epoch
            && name
                .to_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s))
        {
            owned_directory(&entry.path())?;
            remove_import(&entry.path())?;
        }
    }
    private_directory(&epoch)?;
    Ok(epoch)
}

fn open_entry(epoch: &Path, key: &str) -> Result<Prefix> {
    ensure!(
        key.len() == 64
            && key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid OCI prefix key"
    );
    let entry = epoch.join(key);
    owned_directory(&entry)?;
    let root = entry.join("root");
    let stat = fs::symlink_metadata(&root)?;
    let receipt = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(entry.join("binding.json"))?;
    let meta = receipt.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0
            && meta.len() <= 1024,
        "invalid OCI prefix receipt"
    );
    let binding: Binding = serde_json::from_reader(receipt)?;
    ensure!(
        binding.key == key
            && binding.layers > 0
            && stat.is_dir()
            && stat.uid() == meta.uid()
            && (stat.dev(), stat.ino()) == (binding.device, binding.inode),
        "OCI prefix binding changed"
    );
    Ok(Prefix {
        root,
        key: binding.key,
        len: binding.layers,
        reused: true,
    })
}

/// The builder must verify the descriptors and apply them in order to the supplied empty root.
/// Readers use that root exclusively as a lower. All suffix and guest writes use private uppers.
pub(super) fn acquire(
    store: &LocalStore,
    platform: &str,
    layers: &[Descriptor],
    build: impl FnOnce(&Path, &[Descriptor]) -> Result<()>,
) -> Result<Option<Prefix>> {
    let selection = std::env::var_os("PVISOR_OCI_PREFIX_LAYERS");
    let suffix_limit = std::env::var_os("PVISOR_OCI_PREFIX_MAX_SUFFIX_LAYER_BYTES");
    let Some(count) = selected_depth(layers, selection.as_deref(), suffix_limit.as_deref())? else {
        return Ok(None);
    };
    let selected = &layers[..count];
    let key = chain_key(platform, selected)?;
    let epoch = current_epoch(store, boot()?)?;
    acquire_selected(store, &epoch, &key, selected, build).map(Some)
}

fn acquire_selected(
    store: &LocalStore,
    epoch: &Path,
    key: &str,
    layers: &[Descriptor],
    build: impl FnOnce(&Path, &[Descriptor]) -> Result<()>,
) -> Result<Prefix> {
    let _owner = store.lock_file(&format!("prefix-{key}"))?;
    let target = epoch.join(key);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            let prefix = open_entry(epoch, key)?;
            ensure!(prefix.len == layers.len(), "OCI prefix layer count changed");
            return Ok(prefix);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    // An interrupted builder's stage is eligible for the normal image staging prune.
    let stage = store.staging()?;
    let root = stage.path().join("root");
    fs::create_dir(&root)?;
    build(&root, layers)?;
    let stat = fs::symlink_metadata(&root)?;
    ensure!(stat.is_dir(), "OCI prefix builder replaced root");
    let binding = Binding {
        key: key.into(),
        layers: layers.len(),
        device: stat.dev(),
        inode: stat.ino(),
    };
    let mut receipt = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.path().join("binding.json"))?;
    receipt.write_all(&serde_json::to_vec(&binding)?)?;
    fs::rename(stage.path(), target).context("publish verified OCI prefix")?;
    let mut prefix = open_entry(epoch, key)?;
    prefix.reused = false;
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    fn layer(n: u8, bytes: u64) -> Descriptor {
        Descriptor {
            media_type: "application/vnd.oci.image.layer.v1.tar+gzip".into(),
            digest: format!("sha256:{n:064x}"),
            size: Some(bytes),
            platform: None,
        }
    }

    #[test]
    fn cut_is_independent_of_task_suffix_and_requires_a_remaining_layer() {
        let mut layers = [layer(1, 3), layer(2, 7), layer(3, 40)];
        assert_eq!(cut(&layers, 10), Some(2));
        assert_eq!(cut(&layers, 11), None);
        assert_eq!(cut(&layers[..1], 2), None);
        let key = chain_key("linux/amd64", &layers[..2]).unwrap();
        layers[2] = layer(42, 9);
        assert_eq!(key, chain_key("linux/amd64", &layers[..2]).unwrap());
        assert_ne!(key, chain_key("linux/arm64", &layers[..2]).unwrap());
        layers.swap(0, 1);
        assert_ne!(key, chain_key("linux/amd64", &layers[..2]).unwrap());
        layers[0].size = None;
        assert_eq!(cut(&layers, 10), None);
    }

    #[test]
    fn explicit_depth_requires_a_positive_count_and_a_remaining_suffix() {
        let mut layers = [layer(1, CUT_BYTES), layer(2, 1), layer(3, 1)];
        assert_eq!(selected_depth(&layers, None, None).unwrap(), Some(1));
        layers[0].size = None;
        assert_eq!(selected_depth(&layers, None, None).unwrap(), None);
        for (value, expected) in [("1", Some(1)), ("2", Some(2)), ("3", None), ("99", None)] {
            assert_eq!(
                selected_depth(&layers, Some(OsStr::new(value)), None).unwrap(),
                expected
            );
        }
        for value in [
            "",
            "0",
            "01",
            "+1",
            "-1",
            "1 ",
            "0x2",
            "18446744073709551616",
        ] {
            let err = selected_depth(&layers, Some(OsStr::new(value)), None).unwrap_err();
            assert!(format!("{err:#}").contains("PVISOR_OCI_PREFIX_LAYERS"));
        }
    }

    #[rstest::rstest]
    #[case::disabled(Some(1000), None, Some(1))]
    #[case::below(Some(99), Some("100"), Some(1))]
    #[case::at_limit(Some(100), Some("100"), None)]
    #[case::above(Some(101), Some("100"), None)]
    #[case::missing(None, Some("100"), Some(1))]
    fn suffix_limit_uses_individual_known_sizes_after_the_selected_prefix(
        #[case] bytes: Option<u64>,
        #[case] limit: Option<&str>,
        #[case] expected: Option<usize>,
    ) {
        let mut layers = [layer(1, CUT_BYTES), layer(2, 99), layer(3, 0)];
        layers[2].size = bytes;
        for split in [None, Some(OsStr::new("1"))] {
            assert_eq!(
                selected_depth(&layers, split, limit.map(OsStr::new)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn suffix_limit_respects_explicit_split_and_validates_even_without_a_prefix() {
        let layers = [layer(1, 1), layer(2, 100), layer(3, 1)];
        let limit = Some(OsStr::new("100"));
        assert_eq!(selected_depth(&layers, None, limit).unwrap(), None);
        assert_eq!(
            selected_depth(&layers, Some(OsStr::new("2")), limit).unwrap(),
            Some(2)
        );
        for invalid in ["", "0", "01", "+1", "-1", "1 ", "18446744073709551616"] {
            let err = selected_depth(&layers, None, Some(OsStr::new(invalid))).unwrap_err();
            assert!(format!("{err:#}").contains("PVISOR_OCI_PREFIX_MAX_SUFFIX_LAYER_BYTES"));
        }
    }

    #[test]
    fn concurrent_imports_build_once_and_keep_original_file_modes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LocalStore::open(directory.path().join("store")).unwrap();
        let epoch = current_epoch(&store, uuid::Uuid::new_v4()).unwrap();
        let layers = [layer(1, 1)];
        let key = chain_key("linux/amd64", &layers).unwrap();
        let calls = AtomicUsize::new(0);
        let barrier = Arc::new(Barrier::new(4));
        let roots = std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..4 {
                workers.push(scope.spawn(|| {
                    barrier.wait();
                    acquire_selected(&store, &epoch, &key, &layers, |root, selected| {
                        assert_eq!(selected.len(), 1);
                        assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
                        fs::write(root.join("tool"), b"verified content")?;
                        fs::set_permissions(root.join("tool"), fs::Permissions::from_mode(0o755))?;
                        Ok(())
                    })
                    .unwrap()
                }));
            }
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(roots.iter().filter(|r| !r.reused).count(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for r in roots {
            assert_eq!(r.key, key);
            assert_eq!(r.len, 1);
            assert_eq!(r.root, epoch.join(&key).join("root"));
            assert_eq!(
                fs::metadata(r.root.join("tool")).unwrap().mode() & 0o777,
                0o755
            );
            assert_eq!(fs::read(r.root.join("tool")).unwrap(), b"verified content");
        }
    }

    #[test]
    fn failed_build_is_invisible_and_changed_cached_root_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = LocalStore::open(directory.path().join("store")).unwrap();
        let epoch = current_epoch(&store, uuid::Uuid::new_v4()).unwrap();
        let layers = [layer(1, 1)];
        let key = chain_key("linux/amd64", &layers).unwrap();
        let result = acquire_selected(&store, &epoch, &key, &layers, |root, _| {
            fs::create_dir(root.join("partial"))?;
            fs::set_permissions(root.join("partial"), fs::Permissions::from_mode(0o555))?;
            anyhow::bail!("interrupted conversion");
        });
        assert!(result.is_err());
        assert!(!epoch.join(&key).exists());
        assert_eq!(fs::read_dir(store.root.join("staging")).unwrap().count(), 0);
        let prefix = acquire_selected(&store, &epoch, &key, &layers, |root, _| {
            assert!(!root.join("partial").exists());
            fs::write(root.join("ok"), b"ok")?;
            Ok(())
        })
        .unwrap();
        fs::rename(&prefix.root, epoch.join("original")).unwrap();
        symlink(epoch.join("original"), &prefix.root).unwrap();
        assert!(
            acquire_selected(&store, &epoch, &key, &layers, |_, _| panic!(
                "trusted a changed root"
            ))
            .is_err()
        );
    }

    #[test]
    fn separate_store_and_new_boot_miss_the_cached_root() {
        let directory = tempfile::tempdir().unwrap();
        let first = LocalStore::open(directory.path().join("first")).unwrap();
        let second = LocalStore::open(directory.path().join("second")).unwrap();
        let this_boot = uuid::Uuid::new_v4();
        let epoch = current_epoch(&first, this_boot).unwrap();
        let layers = [layer(1, 1)];
        let key = chain_key("linux/amd64", &layers).unwrap();
        let prefix = acquire_selected(&first, &epoch, &key, &layers, |_, _| Ok(())).unwrap();
        let other_epoch = current_epoch(&second, this_boot).unwrap();
        assert!(!other_epoch.join(&key).exists());
        assert!(prefix.root.exists());
        let new_epoch = current_epoch(&first, uuid::Uuid::new_v4()).unwrap();
        assert!(!prefix.root.exists() && !new_epoch.join(&key).exists());
    }
}

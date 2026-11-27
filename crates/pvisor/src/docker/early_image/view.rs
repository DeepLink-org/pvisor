//! Keep a host-visible merged image at its original path while a catalog entry or reader uses it.
use super::*;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

const OWNER: &str = "prefix-view-owner";
const RETIRE: &str = "retire-prefix-view";
const SOURCE: &[u8] = b"pvisor-oci-prefix-v1";

pub(super) fn prepare(dir: &Path) -> anyhow::Result<()> {
    if crate::env::cache::experiment_setting(
        "PVISOR_OCI_PREFIX_REUSE",
        crate::env::os("PVISOR_OCI_PREFIX_REUSE").as_deref(),
    )? {
        // Created before mounting: a killed extractor may not yet have written binding.json.
        File::create_new(dir.join(OWNER))?;
    }
    Ok(())
}

pub(super) fn request_retirement(dir: &Path) -> anyhow::Result<()> {
    if dir.join(OWNER).try_exists()? {
        match File::create_new(dir.join(RETIRE)) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub(super) fn serve_until_retired(
    store: &Store,
    id: &str,
    dir: &Path,
    lease: &File,
    view: crate::image::oci::PrefixView,
) -> anyhow::Result<()> {
    let handle = format!("{PREFIX}{id}");
    let retire = dir.join(RETIRE);
    // A short-lived container may already have been removed before publication completed.
    let mut check = true;
    loop {
        if view.has_exited() {
            bail!("cold image prefix view stopped serving {}", dir.display());
        }
        if check || retire.try_exists()? {
            let _guard = store.lock()?;
            let retained = store.all()?.iter().any(|i| i.image.cache.handle == handle);
            if retained {
                if retire.try_exists()? {
                    fs::remove_file(&retire)?;
                }
                check = false;
            } else {
                // Catalog lock fences new instances/forks. An already-open ImageRoot may
                // outlive its last catalog entry and keeps a shared directory flock.
                match FileExt::try_lock_exclusive(lease) {
                    Ok(()) => break,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // Linux flock conversion can drop the old shared lock on failure.
                        FileExt::lock_shared(lease)?;
                        check = true;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // Still holding the exclusive import lease. Let FUSE drain outside the catalog lock.
    view.unmount()
}

fn mount_path(raw: &[u8]) -> anyhow::Result<PathBuf> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            let code = raw.get(i + 1..i + 4).context("invalid mountinfo escape")?;
            let byte = match code {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => bail!("invalid mountinfo escape"),
            };
            decoded.push(byte);
            i += 4;
        } else {
            decoded.push(raw[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(OsStr::from_bytes(&decoded)))
}

/// Check the kernel mount table without stat-ing a possibly disconnected FUSE root.
fn owned_mounts(
    dir: &Path,
    mountinfo: &[u8],
    prefix_enabled: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let roots = dir.join("rootfs-v3/sha256");
    for line in mountinfo.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<_> = line.split(|b| *b == b' ').collect();
        let path = mount_path(fields.get(4).context("missing mountinfo mountpoint")?)?;
        if !path.starts_with(dir) {
            continue;
        }
        let sep = fields
            .iter()
            .position(|f| *f == b"-")
            .context("missing mountinfo separator")?;
        let kind = fields.get(sep + 1).context("missing mountinfo fs type")?;
        let name = fields.get(sep + 2).context("missing mountinfo source")?;
        let digest = path.file_name().map(OsStrExt::as_bytes).unwrap_or_default();
        ensure!(
            prefix_enabled
                && path.parent() == Some(roots.as_path())
                && digest.len() == 64
                && digest.iter().all(u8::is_ascii_hexdigit)
                && (kind == &b"fuse".as_slice() || kind.starts_with(b"fuse."))
                && *name == SOURCE,
            "refusing to clean cold import with unexpected mount {}",
            path.display()
        );
        ensure!(
            found.is_empty(),
            "multiple mounts below cold import {}",
            dir.display()
        );
        found.push(path);
    }
    Ok(found)
}

// Caller owns the exclusive directory lease. Never let permission repair descend into a mount.
pub(super) fn remove_unmounted(dir: &Path) -> anyhow::Result<()> {
    // The owner marker precedes every prefix mount and survives a killed worker. An
    // ordinary, exclusively leased import has no mounted view and uses its original GC.
    if !dir.join(OWNER).try_exists()? {
        return Ok(crate::image::store::remove_import(dir)?);
    }
    let mounts = || owned_mounts(dir, &fs::read("/proc/self/mountinfo")?, true);
    for path in mounts()? {
        let mut command = Command::new("fusermount3");
        let status = command.args(["-u", "--"]).arg(&path).status();
        let status = match status {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Command::new("fusermount")
                .args(["-u", "--"])
                .arg(&path)
                .status()?,
            status => status?,
        };
        ensure!(
            status.success() && mounts()?.is_empty(),
            "could not detach stale prefix mount {}",
            path.display()
        );
    }
    crate::image::store::remove_import(dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_cleanup_only_accepts_one_exact_owned_fuse_mount() {
        let owner = Path::new("/tmp/store space/import");
        let root = format!(
            "/tmp/store\\040space/import/rootfs-v3/sha256/{}",
            "a".repeat(64)
        );
        let line = format!("1 2 0:7 / {root} rw - fuse pvisor-oci-prefix-v1 rw\n");
        assert_eq!(owned_mounts(owner, line.as_bytes(), true).unwrap().len(), 1);
        assert!(owned_mounts(owner, line.as_bytes(), false).is_err());
        for wrong in [
            line.replace("pvisor-oci-prefix-v1", "another"),
            line.replace("- fuse", "- ext4"),
            line.replace("rootfs-v3/sha256", "unexpected"),
            line.repeat(2),
        ] {
            assert!(owned_mounts(owner, wrong.as_bytes(), true).is_err());
        }
        assert!(
            owned_mounts(Path::new("/other"), line.as_bytes(), true)
                .unwrap()
                .is_empty()
        );
    }
}

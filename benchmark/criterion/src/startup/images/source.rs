//! Pinned image, immutable input verification and fresh evidence configuration.
use super::process;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub(super) const SOURCE: &str =
    "sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794";
pub(super) const MARKER: &[u8] = b"LAZY_READY ubuntu 26.04\n";
pub(super) const WORKLOAD: &str = ". /etc/os-release; test \"$ID\" = ubuntu; test \"$VERSION_ID\" = 26.04; printf \"LAZY_READY ubuntu %s\\n\" \"$VERSION_ID\"";
pub(super) fn workload() -> Vec<String> {
    process::strings(&["/bin/sh", "-ec", WORKLOAD])
}
pub(super) fn digest(path: &Path) -> Result<String> {
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 1024 * 1024];
    loop {
        let n = input.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(super) fn bytes_digest(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}
pub(super) fn hex(value: &str) -> Result<&str> {
    let value = value
        .strip_prefix("sha256:")
        .context("expected sha256 digest")?;
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "noncanonical digest"
    );
    Ok(value)
}
pub(super) fn freeze(source: &Path, destination: &Path) -> Result<String> {
    ensure!(
        fs::symlink_metadata(source)?.is_file(),
        "artifact must be a regular file: {}",
        source.display()
    );
    let expected = digest(source)?;
    fs::copy(source, destination)?;
    ensure!(
        digest(destination)? == expected && digest(source)? == expected,
        "artifact changed while copying"
    );
    Ok(expected)
}
pub(super) fn tree(root: &Path) -> Result<Value> {
    fn walk(root: &Path, dir: &Path, rows: &mut BTreeMap<String, Value>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 image path")?
                .to_owned();
            if metadata.is_symlink() {
                rows.insert(
                    name,
                    json!({"kind":"symlink","target":fs::read_link(&path)?}),
                );
            } else if metadata.is_dir() {
                walk(root, &path, rows)?;
            } else {
                ensure!(metadata.is_file(), "unsupported prepared-store entry");
                rows.insert(name, json!({"kind":"regular","sha256":digest(&path)?}));
            }
        }
        Ok(())
    }
    let mut rows = BTreeMap::new();
    walk(root, root, &mut rows)?;
    Ok(serde_json::to_value(rows)?)
}

/// A read-only upper must exist as a real directory with no entries at all.
pub(super) fn require_empty_upper(root: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "upper must be a real directory"
    );
    ensure!(
        fs::read_dir(root)?.next().is_none(),
        "read-only trial changed upper"
    );
    Ok(())
}

pub(super) fn verify_store(store: &Path) -> Result<Value> {
    let manifest = store
        .join("metadata/manifests-v1")
        .join(format!("{}.json", hex(SOURCE)?));
    ensure!(
        digest(&manifest)? == hex(SOURCE)?,
        "source manifest hash mismatch"
    );
    let manifest = process::load(&manifest)?;
    ensure!(
        manifest.get("manifests").is_none(),
        "single-platform manifest required"
    );
    let config = manifest.get("config").context("missing config")?;
    let layers = manifest["layers"].as_array().context("missing layers")?;
    for descriptor in std::iter::once(config).chain(layers) {
        let hash = hex(descriptor["digest"]
            .as_str()
            .context("missing descriptor digest")?)?;
        let path = store.join("blobs/sha256").join(hash);
        ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "OCI blob must be regular"
        );
        ensure!(
            fs::metadata(&path)?.len()
                == descriptor["size"].as_u64().context("invalid blob size")?
                && digest(&path)? == hash,
            "OCI blob hash/size mismatch"
        );
    }
    let config = process::load(
        &store
            .join("blobs/sha256")
            .join(hex(config["digest"].as_str().unwrap())?),
    )?;
    ensure!(
        config["os"] == "linux" && config["architecture"] == "amd64",
        "require pinned linux/amd64"
    );
    Ok(manifest)
}
pub(super) struct Config {
    pub output: PathBuf,
    pub lab: PathBuf,
    pub store: PathBuf,
    pub binary_dir: PathBuf,
    pub firmware: PathBuf,
    pub cpus: Vec<usize>,
    pub service_cpus: Vec<usize>,
}
impl Config {
    pub(super) fn from_env(scope: &str) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux") && cfg!(target_arch = "x86_64"),
            "image protocols require Linux x86_64"
        );
        let output = PathBuf::from(
            env::var_os("PVISOR_BENCH_OUTPUT").context("PVISOR_BENCH_OUTPUT required")?,
        );
        let parent = output
            .parent()
            .context("output needs existing parent")?
            .canonicalize()?;
        ensure!(
            parent.components().any(|c| c.as_os_str() == ".data"),
            "evidence must resolve below .data"
        );
        let output = parent.join(output.file_name().context("output leaf required")?);
        fs::create_dir(&output)?;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
        let result = (|| -> Result<Self> {
            let required = |name| -> Result<PathBuf> {
                Ok(
                    PathBuf::from(env::var_os(name).with_context(|| format!("{name} required"))?)
                        .canonicalize()?,
                )
            };
            let lab = required("PVISOR_BENCH_LAB")?;
            let store = required("PVISOR_BENCH_PREPARED_STORE")?;
            ensure!(
                !output.starts_with(&store),
                "evidence cannot be inside source image store"
            );
            for name in [
                "containerd-root",
                "cache",
                "run",
                "registry",
                "conversion",
                "log",
                "tools",
                "context",
            ] {
                ensure!(
                    !output.starts_with(lab.join(name)),
                    "evidence cannot be inside mutable/service lab directories"
                );
            }
            let binary_dir = PathBuf::from(
                env::var_os("PVISOR_BENCH_BINARY_DIR").unwrap_or_else(|| "target/release".into()),
            )
            .canonicalize()?;
            let firmware = if let Some(file) = env::var_os("PVISOR_BENCH_FIRMWARE") {
                PathBuf::from(file).canonicalize()?
            } else {
                binary_dir
                    .join("libkrunfw.so.5")
                    .canonicalize()
                    .context("set PVISOR_BENCH_FIRMWARE for dynamic CLI")?
            };
            let cpus =
                process::cpus(&env::var("PVISOR_BENCH_CPUS").unwrap_or_else(|_| "12,13".into()))?;
            let service_cpus = process::cpus(
                &env::var("PVISOR_BENCH_SERVICE_CPUS").unwrap_or_else(|_| "14,15".into()),
            )?;
            let allowed = process::affinity()?;
            ensure!(
                cpus.len() == 2
                    && service_cpus.len() == 2
                    && cpus.iter().all(|c| !service_cpus.contains(c))
                    && cpus
                        .iter()
                        .chain(&service_cpus)
                        .all(|c| allowed.contains(c)),
                "require disjoint available two-CPU pairs"
            );
            process::save(
                &output.join("protocol.json"),
                &json!({"schema":1,"benchmark":"B-LAZY-STARTUP","scope":scope,
                "source_digest":SOURCE,"workload_argv":workload(),"marker":String::from_utf8_lossy(MARKER),
                "cpus":cpus,"service_cpus":service_cpus,"observer_allowed_cpus":allowed,
                "sampling":"Criterion adaptive iter_custom; one iteration is a randomized paired block",
                "schedule":"SplitMix64 shuffled balanced permutations, seed 20261018",
                "warmups":3,"preflight":true,"cache":"host page cache retained; no global cleanup",
                "budget":"2 CPU quota/2 GiB container tasks vs 2 vCPU/2 GiB guest RAM, not equal enclosing budgets",
                "transport":"local loopback HTTP registry and authenticated plaintext cache; not WAN",
                "status":"preparing","valid":false}),
            )?;
            Ok(Self {
                output: output.clone(),
                lab,
                store,
                binary_dir,
                firmware,
                cpus,
                service_cpus,
            })
        })();
        if let Err(error) = &result {
            process::save(
                &output.join("failure.json"),
                &json!({"valid":false,"error":format!("{error:#}")}),
            )?;
        }
        result
    }
    pub(super) fn cpu_text(&self) -> String {
        self.cpus
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_digests() {
        assert!(hex(SOURCE).is_ok());
        assert!(hex("sha256:ABC").is_err());
        assert!(hex(&format!("sha256:{}", "A".repeat(64))).is_err());
    }
    #[test]
    fn tree_does_not_follow_links() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("file"), b"data").unwrap();
        std::os::unix::fs::symlink("/no/such/file", t.path().join("link")).unwrap();
        let v = tree(t.path()).unwrap();
        assert_eq!(v["link"]["target"], "/no/such/file");
        assert_eq!(v["file"]["sha256"], bytes_digest(b"data"));
    }
    #[test]
    fn readonly_upper_requires_real_existing_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(require_empty_upper(&root.path().join("missing")).is_err());
        let upper = root.path().join("upper");
        fs::create_dir(&upper).unwrap();
        assert!(require_empty_upper(&upper).is_ok());
        std::os::unix::fs::symlink(&upper, root.path().join("link")).unwrap();
        assert!(require_empty_upper(&root.path().join("link")).is_err());
        fs::create_dir(upper.join("empty-child")).unwrap();
        assert!(require_empty_upper(&upper).is_err());
    }
}

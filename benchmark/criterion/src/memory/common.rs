use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub experiment: String,
    pub inputs: PathBuf,
    pub output: PathBuf,
    #[serde(default)]
    pub docker_inputs: Option<PathBuf>,
    /// Explicit observed phase metric. No elapsed-time substitution.
    pub metric: String,
    #[serde(default = "seed")]
    pub seed: u64,
    /// One correctness-only matrix in a distinct fresh directory; no statistics.
    #[serde(default)]
    pub preflight: bool,
}
fn seed() -> u64 {
    20261018
}
fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.experiment == "allocator_reclaim" {
        ensure!(
            [
                "64-phys-footprint-extra-drop",
                "256-phys-footprint-extra-drop",
                "64-resident-extra-drop",
                "256-resident-extra-drop"
            ]
            .contains(&cfg.metric.as_str()),
            "unknown allocator byte metric"
        );
    } else {
        let parts = cfg.metric.split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 5,
            "metric must be PATTERN/REFERENCE/CANDIDATE/PHASE/charged|pss"
        );
        ensure!(
            ["repeated", "random-shared", "random-unique"].contains(&parts[0]),
            "unknown content pattern"
        );
        ensure!(
            ["charged", "pss"].contains(&parts[4]),
            "memory statistics must use observed bytes"
        );
        if cfg.experiment == "scanner_compare" {
            ensure!(
                ["off", "ksm", "adaptive"].contains(&parts[1])
                    && ["off", "ksm", "adaptive"].contains(&parts[2]),
                "unknown scanner arm"
            );
            ensure!(
                ["ready", "cow25", "cow100"].contains(&parts[3]),
                "unknown scanner memory phase"
            );
        } else {
            ensure!(
                cfg.docker_inputs.is_some(),
                "full pool/library matrix requires docker_inputs"
            );
            let arms = if cfg.experiment == "library_compare" {
                &[
                    "firecracker-block",
                    "docker-rootless-library",
                    "pvisor-rootfs-off",
                    "pvisor-rootfs-ksm",
                    "pvisor-rootfs-physical-no-hints",
                    "pvisor-rootfs-physical-hints",
                    "pvisor-dax",
                ][..]
            } else {
                &[
                    "firecracker",
                    "docker-rootless",
                    "pvisor-off",
                    "pvisor-ksm",
                    "pvisor-physical",
                    "pvisor-compressed",
                    "pvisor-local",
                ][..]
            };
            ensure!(
                arms.contains(&parts[1]) && arms.contains(&parts[2]),
                "unknown pool/library arm"
            );
            ensure!(
                [
                    "idle-0",
                    "idle-1",
                    "idle-2",
                    "idle-5",
                    "idle-10",
                    "idle-20",
                    "idle-40",
                    "idle-60",
                    "readback",
                    "cow-25",
                    "cow-100",
                    "post-write-1",
                    "post-write-2",
                    "post-write-5",
                    "post-write-10",
                    "post-write-20",
                    "post-write-30"
                ]
                .contains(&parts[3]),
                "unknown curve phase"
            );
            ensure!(
                cfg.experiment != "library_compare" || parts[0] == "random-shared",
                "library cohort is shared-random only"
            );
        }
        ensure!(parts[1] != parts[2], "reference and candidate must differ");
    }
    Ok(())
}

pub fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_reader(
        File::open(p).with_context(|| format!("open {}", p.display()))?,
    )?)
}
pub fn save<T: Serialize + ?Sized>(p: &Path, value: &T) -> Result<()> {
    let temporary = p.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, p)?;
    Ok(())
}
pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Manifest binds modes and links as well as bytes; never follows directory links.
pub fn inventory(root: &Path) -> Result<BTreeMap<String, Value>> {
    fn walk(path: &Path, relative: &Path, out: &mut BTreeMap<String, Value>) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let m = fs::symlink_metadata(path)?;
        let v = if m.file_type().is_symlink() {
            json!({"mode":m.mode(),"link":fs::read_link(path)?,
                "resolved_sha256":if path.is_file(){Some(hash(path)?)}else{None}})
        } else if m.is_file() {
            json!({"mode":m.mode(),"bytes":m.len(),"sha256":hash(path)?})
        } else if m.is_dir() {
            json!({"mode":m.mode(),"directory":true})
        } else {
            bail!("unsupported input object {}", path.display())
        };
        out.insert(relative.to_string_lossy().into_owned(), v);
        if m.is_dir() {
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                walk(&e.path(), &relative.join(e.file_name()), out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new("."), &mut out)?;
    Ok(out)
}

pub use super::process::OwnedChild;
pub fn command(args: &[String], seconds: u64) -> Result<String> {
    ensure!(!args.is_empty(), "empty command");
    // File-backed stdout avoids pipe deadlock for arbitrary diagnostic output.
    let mut out = tempfile::tempfile()?;
    let err = tempfile::tempfile()?;
    let mut child = OwnedChild::spawn(
        Command::new(&args[0])
            .args(&args[1..])
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(err.try_clone()?),
    )?;
    let status = child.wait(seconds)?;
    use std::io::{Seek, SeekFrom};
    out.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    out.read_to_string(&mut text)?;
    ensure!(status.success(), "command {args:?} failed: {status}");
    Ok(text)
}
pub fn cmd(args: &[&str], seconds: u64) -> Result<String> {
    command(
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        seconds,
    )
}
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    pub fn shuffle<T>(&mut self, values: &mut [T]) {
        for i in (1..values.len()).rev() {
            let j = self.next() as usize % (i + 1);
            values.swap(i, j);
        }
    }
}
pub fn median(mut values: Vec<f64>) -> Result<f64> {
    ensure!(
        !values.is_empty() && values.iter().all(|v| v.is_finite()),
        "invalid median input"
    );
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Ok((values[(n - 1) / 2] + values[n / 2]) / 2.0)
}

pub struct Run {
    pub cfg: Config,
    pub index: u64,
    inputs: BTreeMap<String, Value>,
    docker: Option<BTreeMap<String, Value>>,
    harness: BTreeMap<String, Value>,
    worker: Option<(PathBuf, String)>,
}
fn harness_inventory() -> Result<BTreeMap<String, Value>> {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = inventory(&crate_root.join("src/memory"))?;
    for name in [
        "native_worker.rs",
        "pool_vm.rs",
        "allocator_reclaim_vm.rs",
        "docker_init.rs",
    ] {
        let path = crate_root.join("../memory").join(name);
        out.insert(format!("fixture:{name}"), json!({"sha256":hash(&path)?}));
    }
    Ok(out)
}
impl Run {
    pub fn new(mut cfg: Config) -> Result<Self> {
        ensure!(
            [
                "allocator_reclaim",
                "scanner_compare",
                "pool_compare",
                "library_compare"
            ]
            .contains(&cfg.experiment.as_str()),
            "unknown memory experiment"
        );
        validate_config(&cfg)?;
        cfg.inputs = fs::canonicalize(&cfg.inputs)?;
        if let Some(p) = cfg.docker_inputs.as_mut() {
            *p = fs::canonicalize(&*p)?;
        }
        if let Some(output) = std::env::var_os("PVISOR_BENCH_OUTPUT") {
            cfg.output = PathBuf::from(output);
        }
        // Every invocation gets a NEW directory; never overwrite historical cohorts.
        fs::create_dir(&cfg.output).context("output must be a new directory")?;
        cfg.output = fs::canonicalize(&cfg.output)?;
        ensure!(
            cfg.output.as_os_str().len() < 50,
            "short output path required for VM sockets"
        );
        let inputs = inventory(&cfg.inputs)?;
        let docker = cfg.docker_inputs.as_deref().map(inventory).transpose()?;
        let worker = if cfg.experiment == "allocator_reclaim" {
            None
        } else {
            let path = std::env::var_os("PVISOR_MEMORY_WORKER")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_exe()?.with_file_name("pvisor-memory-worker"));
            let path =
                fs::canonicalize(path).context("build pvisor-memory-worker before sampling")?;
            Some((path.clone(), hash(&path)?))
        };
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/memory");
        let harness = harness_inventory()?;
        let frozen = cfg.output.join("harness");
        fs::create_dir(&frozen)?;
        for name in harness.keys().filter(|n| n.ends_with(".rs")) {
            let (path, name) = if let Some(name) = name.strip_prefix("fixture:") {
                (
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../memory")
                        .join(name),
                    name,
                )
            } else {
                (source.join(name), name.as_str())
            };
            fs::copy(path, frozen.join(Path::new(name).file_name().unwrap()))?;
        }
        save(
            &cfg.output.join("manifest.json"),
            &json!({
                "schema":"pvisor-criterion-memory/v1","config":cfg,"inputs":inputs,
                "docker_inputs":docker,"harness":harness,
                "native_worker":worker,
                "host":cmd(&["uname","-a"],5)?,
                "git_head":cmd(&["git","rev-parse","HEAD"],5)?,
                "git_status":cmd(&["git","status","--porcelain=v1"],5)?,
                "executable_sha256":hash(&std::env::current_exe()?)?,
                "statistical_unit":"one randomized fresh-environment condition matrix",
                "preflight":cfg.preflight,
                "raw_scope":"includes all Criterion calibration, warmup and sampling invocations; never pool these as formal samples",
                "cache":"host cache retained; no sysfs or global cache writes"
            }),
        )?;
        Ok(Self {
            cfg,
            index: 0,
            inputs,
            docker,
            harness,
            worker,
        })
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            inventory(&self.cfg.inputs)? == self.inputs,
            "frozen input bytes/modes/links changed"
        );
        if let Some(root) = &self.cfg.docker_inputs {
            ensure!(
                Some(inventory(root)?) == self.docker,
                "Docker inputs changed"
            );
        }
        ensure!(
            harness_inventory()? == self.harness,
            "measurement source changed"
        );
        if let Some((path, wanted)) = &self.worker {
            ensure!(&hash(path)? == wanted, "native worker executable changed");
        }
        Ok(())
    }
    pub fn round(&mut self) -> Result<f64> {
        self.verify()?;
        let root = self.cfg.output.join(format!("round-{:06}", self.index));
        fs::create_dir(&root)?;
        let result = if self.cfg.experiment == "allocator_reclaim" {
            super::allocator::round(&self.cfg, &root, self.index)
        } else {
            #[cfg(target_os = "linux")]
            {
                super::linux::round(&self.cfg, &root, self.index)
            }
            #[cfg(not(target_os = "linux"))]
            {
                bail!("scanner/pool/library require Linux cgroup v2 and systemd")
            }
        };
        match result {
            Ok(value) => {
                self.verify()?;
                save(
                    &root.join("result.json"),
                    &json!({"complete":true,"metric":self.cfg.metric,"value_bytes":value}),
                )?;
                self.index += 1;
                Ok(value)
            }
            Err(error) => {
                save(
                    &root.join("failure.json"),
                    &json!({"complete":false,"error":format!("{error:#}")}),
                )?;
                Err(error)
            }
        }
    }
    pub fn finish(&self) -> Result<()> {
        self.verify()?;
        save(
            &self.cfg.output.join("complete.json"),
            &json!({"complete":true,"raw_rounds":self.index,
            "preflight":self.cfg.preflight,
            "criterion_statistics":"separate Criterion output; raw warmup/calibration rounds are not additional independent formal samples"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shuffle_is_reproducible_and_complete() {
        let mut a = (0..21).collect::<Vec<_>>();
        let mut b = a.clone();
        Rng(42).shuffle(&mut a);
        Rng(42).shuffle(&mut b);
        assert_eq!(a, b);
        a.sort();
        assert_eq!(a, (0..21).collect::<Vec<_>>());
    }
    #[test]
    fn median_retains_negative_values() {
        assert_eq!(median(vec![-8., -2.]).unwrap(), -5.);
    }
    #[test]
    fn changed_mode_is_an_input_change() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("file");
        fs::write(&p, b"same").unwrap();
        let before = inventory(d.path()).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o400)).unwrap();
        assert_ne!(before, inventory(d.path()).unwrap());
    }
}

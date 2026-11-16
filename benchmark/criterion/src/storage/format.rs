//! B-IMAGE-FORMAT-ENG: same frozen ignored runner, authenticated synthetic input.
use super::evidence::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sha3::{
    Shake256,
    digest::{ExtendableOutput, Update, XofReader},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::PathBuf,
};

const TEST: &str = "image::cache::portable::measurement::image_format_measurement";
pub(super) struct Format {
    config: Config,
    source: PathBuf,
    publisher: PathBuf,
    selected: Vec<Vec<u8>>,
    handles: BTreeMap<String, Value>,
    binaries: BTreeMap<String, PathBuf>,
}
impl Format {
    pub fn prepare(e: &mut Evidence, config: &Config) -> Result<Self> {
        ensure!(
            config.files > 0 && (1..=config.files).contains(&config.directories),
            "require 1 <= directories <= files"
        );
        ensure!(
            ["repeat", "entropy"].contains(&config.payload.as_str()),
            "unknown payload"
        );
        ensure!(
            ["pool", "filesystem"].contains(&config.backend.as_str()),
            "unknown backend"
        );
        if let Some(cpu) = config.cpu {
            let mut available: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe {
                    libc::sched_getaffinity(0, std::mem::size_of_val(&available), &mut available)
                } == 0,
                "get affinity"
            );
            ensure!(
                cpu < libc::CPU_SETSIZE as usize && unsafe { libc::CPU_ISSET(cpu, &available) },
                "requested CPU unavailable"
            );
            let mut selected = unsafe { std::mem::zeroed() };
            unsafe {
                libc::CPU_ZERO(&mut selected);
                libc::CPU_SET(cpu, &mut selected);
            }
            ensure!(
                unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&selected), &selected) }
                    == 0,
                "set affinity"
            );
        }
        let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/pvisor/src/image/cache/portable/measurement.rs");
        let normalized = e.root.join("normalized-runner.rs");
        fs::copy(&config.baseline_runner, &normalized)?;
        fs::copy(&config.baseline_runner, e.root.join("baseline-runner.rs"))?;
        fs::copy(&runner, e.root.join("candidate-runner.rs"))?;
        e.command(
            args(&["rustfmt", "--edition", "2021", &s(&normalized)]),
            &BTreeMap::new(),
            30,
            false,
        )?;
        ensure!(
            fs::read(&normalized)? == fs::read(&runner)?,
            "baseline/candidate runner differs beyond formatting"
        );
        let mut binaries = BTreeMap::new();
        for (name, path) in [
            ("baseline", &config.baseline),
            ("candidate", &config.candidate),
        ] {
            let destination = e.root.join(name);
            fs::copy(path, &destination)?;
            e.manifest[format!("{name}_sha256")] = json!(sha(&destination)?);
            binaries.insert(name.to_owned(), destination);
        }
        let publisher = e.root.join("publisher");
        let source = publisher.join("rootfs-v3/sha256").join("a".repeat(64));
        fs::create_dir_all(&source)?;
        let directories = (0..config.directories)
            .map(|i| source.join(format!("d-{i:05}")))
            .collect::<Vec<_>>();
        for directory in &directories {
            fs::create_dir(directory)?;
        }
        let mut paths = vec![];
        for index in 0..config.files {
            let path = directories[index % directories.len()].join(format!("f-{index:07}.py"));
            let mut payload = vec![0u8; 1024];
            if config.payload == "repeat" {
                let digest = Sha256::digest(index.to_string().as_bytes());
                for chunk in payload.chunks_mut(32) {
                    chunk.copy_from_slice(&digest);
                }
            } else {
                let mut shake = Shake256::default();
                Update::update(&mut shake, index.to_string().as_bytes());
                shake.finalize_xof().read(&mut payload);
            }
            fs::write(&path, &payload)?;
            fixed_mtime(&path)?;
            paths.push(path.strip_prefix(&source)?.as_os_str().as_bytes().to_vec());
        }
        for path in std::iter::once(&source).chain(directories.iter()) {
            fixed_mtime(path)?;
        }
        let mut rng = Rng(439);
        for i in (1..paths.len()).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            paths.swap(i, j);
        }
        paths.truncate(1000);
        let mut format = Self {
            config: config.clone(),
            source,
            publisher,
            selected: paths,
            handles: BTreeMap::new(),
            binaries,
        };
        let order = if Rng(431).next() & 1 == 0 {
            ["baseline", "candidate"]
        } else {
            ["candidate", "baseline"]
        };
        let mut publication = json!({});
        for name in order {
            let result = format.command(e, name, "publish")?;
            ensure!(
                result["handle"].as_str().is_some(),
                "publication handle missing"
            );
            format.handles.insert(name.into(), result["handle"].clone());
            publication[name] = result;
        }
        save(&e.root.join("publication.json"), &publication)?;
        let mut inventories = json!({});
        for name in ["baseline", "candidate"] {
            let root = e.root.join(format!("cache-{name}"));
            let mut rows = vec![];
            let mut categories = json!({"data":{"objects":0,"bytes":0,"allocated":0},"metadata":{"objects":0,"bytes":0,"allocated":0}});
            for path in walk(&root)? {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_file() {
                    continue;
                }
                let relative = path.strip_prefix(&root)?;
                let category = if relative.components().next().unwrap().as_os_str() == "data" {
                    "data"
                } else {
                    "metadata"
                };
                for (key, amount) in [
                    ("objects", 1),
                    ("bytes", metadata.len()),
                    ("allocated", metadata.blocks() * 512),
                ] {
                    categories[category][key] =
                        json!(categories[category][key].as_u64().unwrap() + amount);
                }
                rows.push(json!({"key":relative,"bytes":metadata.len(),"allocated":metadata.blocks()*512,"sha256":sha(&path)?}));
            }
            save(&e.root.join(format!("objects-{name}.json")), &rows)?;
            inventories[name] = categories;
        }
        save(&e.root.join("inventory.json"), &inventories)?;
        let mut frozen = walk(&format.source)?;
        frozen.extend([
            e.root.join("baseline-runner.rs"),
            e.root.join("candidate-runner.rs"),
            e.root.join("baseline"),
            e.root.join("candidate"),
        ]);
        e.freeze_files(frozen)?;
        Ok(format)
    }
    fn command(&self, e: &mut Evidence, name: &str, action: &str) -> Result<Value> {
        let directory = e.trial(&format!("{action}-{name}"))?;
        let plan = json!({"action":action,"backend":self.config.backend,"publisher":self.publisher,"source":self.source,
            "cache":e.root.join(format!("cache-{name}")),"output":directory.join("result.json"),"paths":self.selected,
            "entries":self.config.files+self.config.directories,"handle":self.handles.get(name)});
        save(&directory.join("plan.json"), &plan)?;
        let env = BTreeMap::from([(
            "PVISOR_IMAGE_MEASUREMENT".into(),
            s(&directory.join("plan.json")),
        )]);
        e.command(
            args(&[
                &s(&self.binaries[name]),
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
            ]),
            &env,
            1800,
            false,
        )?;
        let value = load(&directory.join("result.json"))?;
        if action == "read" {
            ensure!(
                value["entries"] == (self.config.files + self.config.directories) as u64,
                "enumeration oracle mismatch"
            );
            for key in [
                "open_ns",
                "cold_stat_ns",
                "warm_stat_ns",
                "cold_read_ns",
                "warm_read_ns",
                "scan_ns",
            ] {
                ensure!(
                    value[key].as_u64().is_some_and(|n| n > 0),
                    "missing stage timing {key}"
                );
            }
        }
        Ok(value)
    }
    pub fn pair(&mut self, e: &mut Evidence) -> Result<Value> {
        let order = if e.rng.next() & 1 == 0 {
            ["baseline", "candidate"]
        } else {
            ["candidate", "baseline"]
        };
        let mut pair = json!({"order":order});
        for name in order {
            pair[name] = self.command(e, name, "read")?;
        }
        for name in ["baseline", "candidate"] {
            ensure!(
                sha(&self.binaries[name])?
                    == e.manifest[format!("{name}_sha256")].as_str().unwrap(),
                "frozen artifact changed"
            );
        }
        let directory = e.trial("format-pair")?;
        save(&directory.join("pair.json"), &pair)?;
        Ok(pair)
    }
}
fn fixed_mtime(path: &std::path::Path) -> Result<()> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let times = [libc::timespec {
        tv_sec: 1700000000,
        tv_nsec: 0,
    }; 2];
    ensure!(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "set fixture timestamp"
    );
    Ok(())
}

//! Native Docker / compressed image pool inventories and complete-product
//! resource experiments.
use super::evidence::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Env = BTreeMap<String, String>;

pub(super) struct Docker {
    pub argv: Vec<String>,
    pub pid: u32,
    runtime: PathBuf,
    child: Option<OwnedChild>,
    unit: Option<String>,
}
impl Docker {
    pub fn new(
        e: &mut Evidence,
        directory: &Path,
        slice: Option<&str>,
        insecure: &[String],
    ) -> Result<Self> {
        // Unix socket path must stay short even for deeply nested evidence roots.
        let runtime = std::env::temp_dir().join(format!("pvnd-{}", unique()));
        fs::create_dir(&runtime)?;
        let host = format!("unix://{}", runtime.join("d.sock").display());
        let mut config = json!({"features":{"containerd-snapshotter":true}});
        if !insecure.is_empty() {
            config["insecure-registries"] = json!(insecure);
        }
        if let Some(slice) = slice {
            config["cgroup-parent"] = json!(slice);
            config["exec-opts"] = json!(["native.cgroupdriver=systemd"]);
        }
        save(&runtime.join("daemon.json"), &config)?;
        let env: Env = [
            ("XDG_RUNTIME_DIR".into(), s(&runtime)),
            (
                "DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR".into(),
                s(&runtime.join("kit")),
            ),
            ("DOCKERD_ROOTLESS_ROOTLESSKIT_NET".into(), "pasta".into()),
            (
                "DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS".into(),
                "false".into(),
            ),
        ]
        .into();
        let argv = vec![
            "dockerd-rootless.sh".into(),
            format!("--config-file={}", runtime.join("daemon.json").display()),
            format!("--host={host}"),
            format!("--data-root={}", directory.join("docker").display()),
            format!("--exec-root={}", runtime.join("exec").display()),
            format!("--pidfile={}", runtime.join("d.pid").display()),
            "--iptables=false".into(),
            "--ip6tables=false".into(),
            "--bridge=none".into(),
            "--userland-proxy=false".into(),
        ];
        let mut docker = Self {
            argv: vec!["docker".into(), "--host".into(), host],
            pid: 0,
            runtime,
            child: None,
            unit: None,
        };
        let result = (|| -> Result<()> {
            if let Some(slice) = slice {
                let unit = format!("{}-docker.service", slice.trim_end_matches(".slice"));
                docker.unit = Some(unit.clone());
                let mut command = args(&[
                    "systemd-run",
                    "--user",
                    "--quiet",
                    &format!("--unit={unit}"),
                    &format!("--slice={slice}"),
                    "-p",
                    "Delegate=yes",
                    "-p",
                    "TimeoutStopSec=15s",
                ]);
                command.extend(env.iter().map(|(k, v)| format!("--setenv={k}={v}")));
                command.extend(args(&["taskset", "-c", "0,1,2,3"]));
                command.extend(argv);
                e.command(command, &Env::new(), 20, false)?;
            } else {
                docker.child = Some(spawn_logged(
                    &argv,
                    &env,
                    &directory.join("daemon.stdout"),
                    &directory.join("daemon.stderr"),
                )?);
            }
            let deadline = Instant::now() + Duration::from_secs(45);
            loop {
                let mut cmd = docker.argv.clone();
                cmd.extend(args(&["info", "--format", "{{json .}}"]));
                let info = e.command(cmd, &Env::new(), 5, true)?;
                if let Ok(info) = serde_json::from_str::<Value>(&info) {
                    ensure!(
                        info["Driver"] == "overlayfs",
                        "Docker must use containerd overlayfs"
                    );
                    ensure!(
                        info["DriverStatus"]
                            .as_array()
                            .context("driver status")?
                            .contains(&json!(["driver-type", "io.containerd.snapshotter.v1"])),
                        "not containerd snapshotter"
                    );
                    ensure!(
                        info["SecurityOptions"]
                            .as_array()
                            .context("security options")?
                            .contains(&json!("name=rootless")),
                        "not rootless"
                    );
                    docker.pid = fs::read_to_string(docker.runtime.join("d.pid"))?
                        .trim()
                        .parse()?;
                    save(&directory.join("docker-info.json"), &info)?;
                    return Ok(());
                }
                ensure!(
                    Instant::now() < deadline,
                    "private Docker readiness timeout"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        })();
        if result.is_err() {
            docker.close(e);
        }
        result?;
        Ok(docker)
    }
    pub fn close(&mut self, e: &mut Evidence) {
        if let Some(unit) = self.unit.take() {
            let _ = e.command(
                args(&[
                    "journalctl",
                    "--user",
                    &format!("--unit={unit}"),
                    "--no-pager",
                ]),
                &Env::new(),
                20,
                true,
            );
            if let Err(error) = e.command(
                args(&["systemctl", "--user", "stop", &unit]),
                &Env::new(),
                30,
                false,
            ) {
                e.cleanup_error(error);
            }
        }
        if let Some(mut child) = self.child.take() {
            if let Err(error) = stop(&mut child) {
                e.cleanup_error(error);
            }
        }
        let _ = fs::remove_dir_all(&self.runtime);
    }
}
pub(super) fn import(
    e: &mut Evidence,
    image: &Value,
    backend: &str,
    prefix: &[String],
    env: &Env,
) -> Result<()> {
    let pinned = image["pinned"].as_str().context("pinned")?;
    let mut argv = prefix.to_vec();
    argv.push("pull".into());
    if backend == "docker" {
        argv.push("--platform=linux/amd64".into());
    }
    argv.push(pinned.into());
    e.command(argv, env, 600, false)?;
    if backend == "docker" {
        let mut argv = prefix.to_vec();
        argv.extend(args(&["image", "inspect", pinned]));
        let values: Value = serde_json::from_str(&e.command(argv, env, 30, false)?)?;
        let actual = &values[0];
        let config = load(
            &Path::new(image["source"].as_str().unwrap()).join(digest_hex(
                image["manifest"]["config"]["digest"].as_str().unwrap(),
            )?),
        )?;
        ensure!(
            actual["Descriptor"]["digest"] == image["digest"]
                && actual["RootFS"]["Layers"] == config["rootfs"]["diff_ids"],
            "Docker image identity mismatch"
        );
        ensure!(
            actual["RepoDigests"]
                .as_array()
                .context("repo digests")?
                .contains(&json!(pinned)),
            "Docker reference mismatch"
        );
    } else {
        let images = load(
            &Path::new(env.get("PVISOR_DOCKER_HOME").context("private store")?).join("images.json"),
        )?;
        let actual = images
            .as_array()
            .context("images list")?
            .iter()
            .find(|i| i["reference"] == pinned)
            .context("image not present")?;
        ensure!(
            actual["digest"] == image["digest"]
                && actual["id"] == image["manifest"]["config"]["digest"],
            "pVisor identity mismatch"
        );
    }
    Ok(())
}
// Directory, handle prefix and content subdirectory of each pool an inventory
// accepts. A store holding both is not measured.
const POOLS: [(&str, &str, &str); 2] = [
    ("image-pack-v1", "pvisor-image-pack:v1:", "blobs"),
    ("pool-v2", "pvisor-v2:", "packs"),
];
pub(super) fn pool_details(store: &Path) -> Result<Value> {
    let images = load(&store.join("images.json"))?;
    for name in ["blobs", "rootfs-v3", "built"] {
        ensure!(
            !store.join("images").join(name).exists(),
            "unexpected image tree"
        );
    }
    let mut pools = Vec::new();
    for pool in POOLS {
        let root = store.join("images").join(pool.0);
        if root.join("format.json").exists() {
            pools.push((pool, root));
        }
    }
    ensure!(
        pools.len() == 1,
        "expected one compressed image pool, found {}",
        pools.len()
    );
    let (pool, root) = &pools[0];
    let marker = load(&root.join("format.json"))?;
    let supported = match pool.0 {
        "image-pack-v1" => marker["format"] == "pvisor-image-pack" && marker["format_version"] == 1,
        _ => marker["format_version"] == 2 && marker["encoding"] == "zstd-seekable",
    };
    ensure!(supported, "unsupported {} pool marker", pool.0);
    let bound = |image: &Value| {
        image.get("rootfs").is_none_or(|r| r == "")
            && image["cache"]["handle"]
                .as_str()
                .is_some_and(|h| h.starts_with(pool.1))
    };
    ensure!(
        images.as_array().context("images")?.iter().all(bound),
        "uncompressed image store"
    );
    let files = |directory: &Path| {
        walk(directory).map(|paths| paths.into_iter().filter(|p| p.is_file()).count())
    };
    let (objects, content) = (
        files(&root.join("data"))?,
        files(&root.join("data").join(pool.2))?,
    );
    Ok(match pool.0 {
        "image-pack-v1" => json!({"format":pool.0,"objects":objects,"blobs":content,"packs":0}),
        _ => json!({"format":pool.0,"objects":objects,"blobs":0,"packs":content}),
    })
}
pub(super) fn disk(
    e: &mut Evidence,
    worker: &Path,
    images: &[Value],
    label: &str,
) -> Result<Value> {
    let directory = e.trial(label)?;
    let mut docker = Docker::new(e, &directory, None, &[])?;
    let store = directory.join("pvisor");
    let env = Env::from([("PVISOR_DOCKER_HOME".into(), s(&store))]);
    let empty_env = Env::new();
    let result = (|| -> Result<Value> {
        let empty = namespace_inventory(
            e,
            worker,
            &directory.join("docker"),
            docker.pid,
            &directory.join("docker-empty.json"),
        )?;
        let order = e.rng.order();
        for backend in order {
            for image in images {
                let prefix = if backend == "docker" {
                    docker.argv.clone()
                } else {
                    vec![s(&e.binary)]
                };
                import(
                    e,
                    image,
                    backend,
                    &prefix,
                    if backend == "docker" {
                        &empty_env
                    } else {
                        &env
                    },
                )?;
            }
        }
        let pv = inventory(&store)?;
        save(&directory.join("pvisor-inventory.json"), &pv)?;
        let dk = namespace_inventory(
            e,
            worker,
            &directory.join("docker"),
            docker.pid,
            &directory.join("docker-inventory.json"),
        )?;
        let content = namespace_inventory(
            e,
            worker,
            &directory.join("docker/containerd/daemon/io.containerd.content.v1.content"),
            docker.pid,
            &directory.join("docker-content.json"),
        )?;
        let snapshots = namespace_inventory(
            e,
            worker,
            &directory.join("docker/containerd/daemon/io.containerd.snapshotter.v1.overlayfs"),
            docker.pid,
            &directory.join("docker-snapshots.json"),
        )?;
        for image in images {
            let mut outputs = vec![];
            for backend in ["docker", "pvisor"] {
                let mut argv = if backend == "docker" {
                    docker.argv.clone()
                } else {
                    vec![s(&e.binary)]
                };
                argv.extend(args(&[
                    "run",
                    "--rm",
                    "--network=none",
                    "--cpus=1",
                    "--memory=256m",
                    image["pinned"].as_str().unwrap(),
                    "/bin/sh",
                    "-ec",
                    "sha256sum /etc/os-release /bin/sh",
                ]));
                outputs.push(e.command(
                    argv,
                    if backend == "docker" {
                        &empty_env
                    } else {
                        &env
                    },
                    90,
                    false,
                )?);
            }
            ensure!(
                outputs[0] == outputs[1],
                "same-image known content mismatch"
            );
        }
        let before = pool_details(&store)?;
        e.command(
            args(&[&s(&e.binary), "pull", images[0]["pinned"].as_str().unwrap()]),
            &env,
            600,
            false,
        )?;
        ensure!(
            pool_details(&store)? == before,
            "repeated pull added objects"
        );
        Ok(
            json!({"label":label,"order":order,"unit":"bytes / inode count; not latency","docker_empty":empty,"pvisor":pv,"docker":dk,"docker_content":content,"docker_snapshots":snapshots,"pool":before,
            "docker_incremental_allocated":dk["allocated"].as_i64().context("allocated")?-empty["allocated"].as_i64().context("empty allocated")?}),
        )
    })();
    save(
        &directory.join("result.json"),
        &match &result {
            Ok(v) => v.clone(),
            Err(err) => json!({"status":"failed","error":format!("{err:#}")}),
        },
    )?;
    docker.close(e);
    result
}
fn namespace_inventory(
    e: &mut Evidence,
    worker: &Path,
    root: &Path,
    pid: u32,
    destination: &Path,
) -> Result<Value> {
    let output = e.command(
        args(&[
            "nsenter",
            "-t",
            &pid.to_string(),
            "-U",
            "-m",
            "--",
            &s(worker),
            "inventory",
            &s(root),
        ]),
        &Env::new(),
        120,
        false,
    )?;
    let value = serde_json::from_str(&output)?;
    save(destination, &value)?;
    Ok(value)
}

pub(super) struct Product {
    pub prefix: Vec<String>,
    pub env: Env,
    pub directory: PathBuf,
    pub group: PathBuf,
    pub backend: String,
    pub store: PathBuf,
    pub payload: Vec<String>,
    slice: String,
    client: String,
    docker: Option<Docker>,
}
impl Product {
    pub fn new(e: &mut Evidence, directory: PathBuf, backend: &str, worker: &Path) -> Result<Self> {
        fs::create_dir_all(&directory)?;
        let slice = format!("pvnative{}.slice", unique().replace('-', ""));
        let client = format!("{}-clients.service", slice.trim_end_matches(".slice"));
        let properties = args(&[
            "CPUAccounting",
            "b",
            "true",
            "MemoryAccounting",
            "b",
            "true",
            "CPUQuotaPerSecUSec",
            "t",
            "4000000",
            "MemoryMax",
            "t",
            "4294967296",
            "MemorySwapMax",
            "t",
            "0",
        ]);
        let mut command = args(&[
            "busctl",
            "--user",
            "--quiet",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
            &slice,
            "fail",
            "5",
        ]);
        command.extend(properties);
        command.push("0".into());
        e.command(command, &Env::new(), 20, false)?;
        let mut p = Self {
            prefix: vec![],
            env: Env::new(),
            store: directory.join("pvisor"),
            directory,
            group: PathBuf::new(),
            backend: backend.into(),
            slice,
            client,
            docker: None,
            payload: args(&[
                "/usr/bin/taskset",
                "-c",
                if backend == "docker" { "0,1,2,3" } else { "0" },
            ]),
        };
        let result = (|| -> Result<()> {
            p.group = group_for(e, &p.slice, &p.slice)?;
            e.command(
                args(&[
                    "systemd-run",
                    "--user",
                    "--quiet",
                    &format!("--unit={}", p.client),
                    &format!("--slice={}", p.slice),
                    "-p",
                    "Delegate=yes",
                    "taskset",
                    "-c",
                    "0,1,2,3",
                    "/usr/bin/sleep",
                    "infinity",
                ]),
                &Env::new(),
                20,
                false,
            )?;
            let leaf = group_for(e, &p.client, &p.slice)?;
            p.prefix = args(&[
                &s(worker),
                "enter-group",
                &s(&leaf),
                "taskset",
                "-c",
                "0,1,2,3",
            ]);
            if backend == "docker" {
                let docker = Docker::new(e, &p.directory, Some(&p.slice), &[])?;
                p.prefix.extend(docker.argv.clone());
                p.docker = Some(docker);
            } else {
                p.prefix.push(s(&e.binary));
                p.env.insert("PVISOR_DOCKER_HOME".into(), s(&p.store));
                p.env.insert("PVISOR_STARTUP_TIMING".into(), "0".into());
            }
            p.resources()?;
            Ok(())
        })();
        if result.is_err() {
            p.close(e);
        }
        result?;
        Ok(p)
    }
    pub fn command(&self, e: &mut Evidence, argv: &[&str]) -> Result<String> {
        let mut command = self.prefix.clone();
        command.extend(args(argv));
        e.command(command, &self.env, 600, false)
    }
    pub fn resources(&self) -> Result<Value> {
        let text = |name: &str| -> Result<String> {
            Ok(fs::read_to_string(self.group.join(name))?.trim().into())
        };
        ensure!(text("cgroup.type")? == "domain", "not domain cgroup");
        ensure!(
            text("memory.max")? == "4294967296" && text("memory.swap.max")? == "0",
            "whole-product memory budget changed"
        );
        let quota = text("cpu.max")?
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        ensure!(
            quota.len() == 2 && quota[0].parse::<u64>()? == 4 * quota[1].parse::<u64>()?,
            "whole-product CPU quota changed"
        );
        let counters = |name: &str| -> Result<BTreeMap<String, u64>> {
            let mut values = BTreeMap::new();
            for line in text(name)?.lines() {
                let pair = line.split_whitespace().collect::<Vec<_>>();
                ensure!(
                    pair.len() == 2 && values.insert(pair[0].into(), pair[1].parse()?).is_none(),
                    "invalid cgroup counters"
                );
            }
            Ok(values)
        };
        let events = counters("memory.events")?;
        ensure!(
            events.values().all(|v| *v == 0),
            "memory pressure invalidates cohort"
        );
        ensure!(
            text("memory.swap.current")? == "0",
            "swap use invalidates cohort"
        );
        Ok(
            json!({"cgroup":self.group,"memory_max_bytes":4294967296u64,"swap_max_bytes":0,"cpu_quota":quota,"cpu_placement":"affinity; not cpuset delegation",
            "memory_current_bytes":text("memory.current")?.parse::<u64>()?,"memory_peak_bytes":text("memory.peak")?.parse::<u64>()?,
            "memory_peak_scope":"cgroup lifetime high-water mark; not a per-task peak, no verified reset",
            "memory_stat":counters("memory.stat")?,"memory_events":events,"cpu_stat":counters("cpu.stat")?,
            "scope":"whole selected cgroup including descendants; excludes shared pages charged elsewhere"}),
        )
    }
    pub fn witness(&self, e: &mut Evidence, names: &[String]) -> Result<Value> {
        let mut required = vec![];
        if let Some(docker) = &self.docker {
            required.push(docker.pid);
            for name in names {
                required.push(
                    self.command(e, &["inspect", "--format", "{{.State.Pid}}", name])?
                        .trim()
                        .parse()?,
                );
            }
        } else if !names.is_empty() {
            let ids = walk(&self.store.join("instances"))?
                .into_iter()
                .filter(|p| p.file_name().is_some_and(|n| n == "instance.json"))
                .map(|p| load(&p).map(|v| v["id"].as_str().unwrap_or("").to_owned()))
                .collect::<Result<Vec<_>>>()?;
            for proc in fs::read_dir("/proc")? {
                let path = proc?.path();
                let Ok(pid) = path.file_name().unwrap().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                let Ok(cmd) = fs::read(path.join("cmdline")) else {
                    continue;
                };
                let argv = cmd.split(|b| *b == 0).collect::<Vec<_>>();
                if argv.first() == Some(&s(&e.binary).as_bytes())
                    && ids.iter().any(|id| argv.contains(&id.as_bytes()))
                {
                    required.push(pid);
                }
            }
            ensure!(required.len() >= 2 * names.len(), "missing live owner/VMM");
        }
        // Retry only an unstable observation, retaining every attempt.
        let mut last = None;
        for attempt in 0..3 {
            let result = witness_group(&self.group, &required);
            save(
                &self
                    .directory
                    .join(format!("witness-{}-{attempt}.json", unique())),
                &match &result {
                    Ok(v) => v.clone(),
                    Err(err) => json!({"error":format!("{err:#}")}),
                },
            )?;
            match result {
                Ok(value) => return Ok(value),
                Err(err) => last = Some(err),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(last.unwrap())
    }
    pub fn observe(&self, e: &mut Evidence, names: &[String], label: &str) -> Result<f64> {
        let start = Instant::now();
        let mut samples = vec![];
        let mut tail = vec![];
        while start.elapsed() < Duration::from_secs(3) {
            let mut value = self.resources()?;
            let elapsed = start.elapsed().as_secs_f64();
            value["elapsed_s"] = json!(elapsed);
            if elapsed >= 1. {
                tail.push(value["memory_current_bytes"].as_f64().unwrap());
            }
            samples.push(value);
            std::thread::sleep(Duration::from_millis(200));
        }
        let witness = self.witness(e, names)?;
        tail.sort_by(f64::total_cmp);
        let median = median(&tail);
        save(
            &self.directory.join(format!("{label}.json")),
            &json!({"samples":samples,"memory_current_bytes":median,"witness":witness}),
        )?;
        Ok(median)
    }
    pub fn close(&mut self, e: &mut Evidence) {
        if !self.prefix.is_empty() {
            let mut cmd = self.prefix.clone();
            cmd.extend(args(&["ps", "-aq"]));
            if let Ok(ids) = e.command(cmd, &self.env, 20, true) {
                if !ids.trim().is_empty() {
                    let mut cmd = self.prefix.clone();
                    cmd.extend(args(&["rm", "-f"]));
                    cmd.extend(ids.split_whitespace().map(str::to_owned));
                    if let Err(error) = e.command(cmd, &self.env, 30, false) {
                        e.cleanup_error(error);
                    }
                }
            }
            let mut check = self.prefix.clone();
            check.extend(args(&["ps", "-aq"]));
            match e.command(check, &self.env, 20, false) {
                Ok(ids) if ids.trim().is_empty() => (),
                Ok(_) => e.cleanup_error(anyhow::anyhow!("instances remain after scoped cleanup")),
                Err(error) => e.cleanup_error(error),
            }
        }
        if let Some(docker) = &mut self.docker {
            docker.close(e);
        }
        if let Err(error) = e.command(
            args(&["systemctl", "--user", "stop", &self.client, &self.slice]),
            &Env::new(),
            30,
            false,
        ) {
            e.cleanup_error(error);
        }
    }
}
fn group_for(e: &mut Evidence, unit: &str, slice: &str) -> Result<PathBuf> {
    let value = e.command(
        args(&[
            "systemctl",
            "--user",
            "show",
            unit,
            "--property=ControlGroup",
            "--value",
        ]),
        &Env::new(),
        10,
        false,
    )?;
    let value = value.trim();
    ensure!(
        value.starts_with("/user.slice/")
            && value.split('/').any(|p| p == slice)
            && !value.contains(".."),
        "foreign cgroup"
    );
    Ok(Path::new("/sys/fs/cgroup").join(value.trim_start_matches('/')))
}
fn members(group: &Path) -> Result<BTreeMap<u32, String>> {
    let mut members = BTreeMap::new();
    for path in
        std::iter::once(group.to_owned()).chain(walk(group)?.into_iter().filter(|p| p.is_dir()))
    {
        for pid in fs::read_to_string(path.join("cgroup.procs"))?.split_whitespace() {
            let pid = pid.parse()?;
            ensure!(
                members
                    .insert(
                        pid,
                        format!("/{}", path.strip_prefix("/sys/fs/cgroup")?.display())
                    )
                    .is_none(),
                "membership changed during scan"
            );
        }
    }
    Ok(members)
}
fn cpus(text: &str) -> Result<BTreeSet<u32>> {
    let mut result = BTreeSet::new();
    for part in text.trim().split(',') {
        let bounds = part.split('-').collect::<Vec<_>>();
        ensure!((1..=2).contains(&bounds.len()), "invalid CPU list");
        let first: u32 = bounds[0].parse()?;
        let last: u32 = bounds.last().unwrap().parse()?;
        ensure!(first <= last && last <= 1048575, "invalid CPU range");
        for cpu in first..=last {
            ensure!(result.insert(cpu), "overlapping CPU range");
        }
    }
    Ok(result)
}
fn identity(path: &Path, pid: u32) -> Result<String> {
    let stat = fs::read_to_string(path.join("stat"))?;
    ensure!(stat.starts_with(&format!("{pid} (")), "PID changed");
    let fields = stat
        .rsplit_once(") ")
        .context("proc stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure!(
        !["Z", "X", "x"].contains(&fields[0]),
        "process exited during witness"
    );
    Ok(fields.get(19).context("start ticks")?.to_string())
}
fn witness_group(group: &Path, required: &[u32]) -> Result<Value> {
    let before = members(group)?;
    ensure!(
        required.iter().all(|p| before.contains_key(p)),
        "required process escaped group"
    );
    let parent = Path::new("/").join(group.strip_prefix("/sys/fs/cgroup")?);
    let mut witnesses = vec![];
    for (&pid, expected) in &before {
        let path = Path::new("/proc").join(pid.to_string());
        let started = identity(&path, pid)?;
        let tids = fs::read_dir(path.join("task"))?
            .map(|e| e.map(|x| x.file_name()))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        ensure!(!tids.is_empty(), "empty thread scope");
        let mut threads = vec![];
        for tid in &tids {
            let task = path.join("task").join(tid);
            let tid = tid.to_string_lossy().parse()?;
            let id = identity(&task, tid)?;
            let state = || -> Result<(String, BTreeSet<u32>)> {
                let cgroup = fs::read_to_string(task.join("cgroup"))?;
                let groups = cgroup
                    .lines()
                    .filter_map(|l| l.strip_prefix("0::"))
                    .collect::<Vec<_>>();
                ensure!(
                    groups.len() == 1
                        && Path::new(groups[0]).starts_with(&parent)
                        && groups[0] == expected,
                    "thread escaped product group"
                );
                let status = fs::read_to_string(task.join("status"))?;
                let allowed = status
                    .lines()
                    .filter_map(|l| l.strip_prefix("Cpus_allowed_list:"))
                    .collect::<Vec<_>>();
                ensure!(allowed.len() == 1, "missing affinity");
                let cpus = cpus(allowed[0])?;
                ensure!(
                    !cpus.is_empty() && cpus.iter().all(|c| *c <= 3),
                    "thread affinity escaped CPUs0-3"
                );
                Ok((groups[0].to_owned(), cpus))
            };
            let a = state()?;
            let b = state()?;
            ensure!(
                a == b && identity(&task, tid)? == id,
                "thread changed during witness"
            );
            threads.push(json!({"tid":tid,"start_ticks":id,"cgroup":a.0,"cpus":a.1}));
        }
        ensure!(identity(&path, pid)? == started, "PID reused");
        let after = fs::read_dir(path.join("task"))?
            .map(|e| e.map(|x| x.file_name()))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        ensure!(tids == after, "thread scope changed");
        witnesses.push(json!({"pid":pid,"start_ticks":started,"threads":threads}));
    }
    ensure!(
        before == members(group)?,
        "membership changed during witness"
    );
    Ok(
        json!({"required_pids":required,"membership":before,"witnesses":witnesses,"scope":"stable live window, not proof of past transient init affinity"}),
    )
}
fn median(values: &[f64]) -> f64 {
    if values.len() % 2 == 0 {
        (values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.
    } else {
        values[values.len() / 2]
    }
}
pub(super) fn runtime_pair(
    e: &mut Evidence,
    products: &BTreeMap<String, Product>,
    image: &Value,
    payload: Option<&[String]>,
) -> Result<Value> {
    let order = e.rng.order();
    let mut pair = json!({"order":order});
    for backend in order {
        let product = &products[backend];
        let directory = e.trial(&format!("runtime-{backend}"))?;
        let mut argv = product.prefix.clone();
        let local = payload.is_some();
        argv.extend(args(&[
            "run",
            "--rm",
            "--network=none",
            if local { "--cpus=2" } else { "--cpus=1" },
            if local {
                "--memory=512m"
            } else {
                "--memory=256m"
            },
            image["pinned"].as_str().unwrap(),
        ]));
        argv.extend(if local {
            args(&["/usr/bin/taskset", "-c", "0,1"])
        } else {
            product.payload.clone()
        });
        argv.extend(args(&["/bin/sh","-ec","test ! -e /tmp/pv-private; printf check >/tmp/pv-private; test \"$(cat /tmp/pv-private)\" = check; exec \"$@\"","probe"]));
        argv.extend(payload.map(|p| p.to_vec()).unwrap_or(args(&[
            "/bin/sh",
            "-ec",
            "printf 'REFERENCE_READY\\n'",
        ])));
        let record = capture(
            &argv,
            &product.env,
            &directory,
            if local { 120 } else { 30 },
            true,
        )?;
        let stdout = fs::read_to_string(directory.join("stdout"))?;
        if !local {
            ensure!(stdout == "REFERENCE_READY\n", "unexpected runtime stdout");
        }
        ensure!(
            product.command(e, &["ps", "-aq"])?.trim().is_empty(),
            "instance cleanup failed"
        );
        let resources = product.resources()?;
        let witness = product.witness(e, &[])?;
        save(
            &directory.join("resources.json"),
            &json!({"observation":resources,"witness":witness}),
        )?;
        let mut metrics = json!({"ready_ns":record["ready_ns"],"exit_ns":record["exit_ns"]});
        let phases = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("READ_PHASES "))
            .collect::<Vec<_>>();
        if stdout.contains("READ_PHASES") {
            ensure!(phases.len() == 1, "exactly one phase record required");
            let phases: Value = serde_json::from_str(phases[0])?;
            ensure!(
                phases.as_object().context("read phases")?.len() == 2,
                "unexpected phase keys"
            );
            for key in ["first_ns", "repeat_ns"] {
                ensure!(
                    phases[key].as_u64().is_some_and(|n| n > 0),
                    "invalid read clock"
                );
                metrics[key] = phases[key].clone();
            }
        }
        save(&directory.join("metrics.json"), &metrics)?;
        pair[backend] = metrics;
    }
    let directory = e.trial("runtime-pair")?;
    save(&directory.join("pair.json"), &pair)?;
    Ok(pair)
}
pub(super) fn memory_pair(e: &mut Evidence, worker: &Path, image: &Value) -> Result<Value> {
    let order = e.rng.order();
    let mut pair = json!({"order":order});
    for backend in order {
        let directory = e.trial(&format!("memory-{backend}"))?;
        let mut product = Product::new(e, directory, backend, worker)?;
        let result = (|| -> Result<Value> {
            import(e, image, backend, &product.prefix, &product.env)?;
            let mut names = vec![];
            let empty = product.observe(e, &names, "empty")?;
            let mut result = json!({"empty":empty});
            for index in 0..4 {
                let name = format!("pvmem-{index}");
                names.push(name.clone());
                let mut argv = product.prefix.clone();
                argv.extend(args(&[
                    "run",
                    "-d",
                    "--name",
                    &name,
                    "--network=none",
                    "--cpus=1",
                    "--memory=256m",
                    image["pinned"].as_str().unwrap(),
                ]));
                argv.extend(product.payload.clone());
                argv.extend(args(&["/bin/sh","-ec",&format!("test ! -e /tmp/pv-private; printf '{name}' >/tmp/pv-private; printf '{name}\\n'; exec /bin/sleep 180")]));
                e.command(argv, &product.env, 30, false)?;
                let deadline = Instant::now() + Duration::from_secs(5);
                while product.command(e, &["logs", &name])? != format!("{name}\n") {
                    ensure!(
                        Instant::now() < deadline,
                        "memory payload readiness timeout"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                for peer in &names {
                    let mut argv = product.prefix.clone();
                    argv.extend(args(&["exec", peer]));
                    argv.extend(product.payload.clone());
                    argv.extend(args(&["/bin/cat", "/tmp/pv-private"]));
                    ensure!(
                        e.command(argv, &product.env, 30, false)? == *peer,
                        "private writes leaked"
                    );
                }
                if index == 0 || index == 3 {
                    let key = if index == 0 { "one" } else { "four" };
                    let memory = product.observe(e, &names, key)?;
                    result[key] = json!(memory);
                    result[format!("{key}_increment")] =
                        json!((memory - empty) / (index + 1) as f64);
                }
            }
            Ok(result)
        })();
        product.close(e);
        pair[backend] = result?;
    }
    let directory = e.trial("memory-pair")?;
    save(&directory.join("pair.json"), &pair)?;
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_parser_rejects_overlap_and_unbounded_ranges() {
        assert_eq!(cpus("0-3").unwrap(), BTreeSet::from([0, 1, 2, 3]));
        for text in ["0-3,2", "3-1", "0-999999999", "", "1-2-3"] {
            assert!(cpus(text).is_err(), "{text}");
        }
    }
    #[test]
    fn median_preserves_fractional_bytes() {
        assert_eq!(median(&[1., 2.]), 1.5);
        assert_eq!(median(&[1., 2., 3.]), 2.);
    }
    const PACK: &str = r#"{"format":"pvisor-image-pack","format_version":1,"packing":{"blob_target_bytes":33554432}}"#;
    const LEGACY: &str = r#"{"format_version":2,"encoding":"zstd-seekable"}"#;
    fn pooled(handle: &str) -> Value {
        json!({"cache": {"handle": handle}})
    }
    fn pool_store(root: &Path, name: &str, pool: &str, marker: &str, image: Value) -> PathBuf {
        let store = root.join(name);
        let content =
            store
                .join("images")
                .join(pool)
                .join("data")
                .join(if pool == "image-pack-v1" {
                    "blobs"
                } else {
                    "packs"
                });
        fs::create_dir_all(content.join("aa/bb")).unwrap();
        fs::write(store.join("images").join(pool).join("format.json"), marker).unwrap();
        fs::write(content.join("aa/bb/object"), b"encoded").unwrap();
        save(&store.join("images.json"), &json!([image])).unwrap();
        store
    }
    #[test]
    fn pool_details_reads_image_pack_v1_and_a_retained_v2_pool() {
        let root = tempfile::tempdir().unwrap();
        let pack = pool_store(
            root.path(),
            "pack",
            "image-pack-v1",
            PACK,
            pooled("pvisor-image-pack:v1:k:linux-amd64:r"),
        );
        assert_eq!(
            pool_details(&pack).unwrap(),
            json!({"format":"image-pack-v1","objects":1,"blobs":1,"packs":0})
        );
        let legacy = pool_store(
            root.path(),
            "legacy",
            "pool-v2",
            LEGACY,
            json!({"rootfs":"","cache":{"handle":"pvisor-v2:k:linux-amd64:r"}}),
        );
        assert_eq!(
            pool_details(&legacy).unwrap(),
            json!({"format":"pool-v2","objects":1,"blobs":0,"packs":1})
        );
    }
    #[test]
    fn pool_details_rejects_unsupported_pools() {
        let root = tempfile::tempdir().unwrap();
        let message = |store: &Path| pool_details(store).unwrap_err().to_string();
        let handle = "pvisor-image-pack:v1:k:linux-amd64:r";
        assert!(
            message(&pool_store(
                root.path(),
                "marker",
                "image-pack-v1",
                LEGACY,
                pooled(handle)
            ))
            .contains("marker")
        );
        assert!(
            message(&pool_store(
                root.path(),
                "handle",
                "image-pack-v1",
                PACK,
                pooled("pvisor-v2:k:linux-amd64:r")
            ))
            .contains("uncompressed")
        );
        assert!(
            message(&pool_store(
                root.path(),
                "rootfs",
                "image-pack-v1",
                PACK,
                json!({"rootfs":"/tree","cache":{"handle":handle}})
            ))
            .contains("uncompressed")
        );
        let none = root.path().join("none");
        fs::create_dir_all(none.join("images")).unwrap();
        save(&none.join("images.json"), &json!([])).unwrap();
        assert!(message(&none).contains("found 0"));
        let both = pool_store(root.path(), "both", "image-pack-v1", PACK, pooled(handle));
        fs::create_dir_all(both.join("images/pool-v2")).unwrap();
        fs::write(both.join("images/pool-v2/format.json"), LEGACY).unwrap();
        assert!(message(&both).contains("found 2"));
    }
}

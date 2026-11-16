//! Linux full-product phase accounting and fresh-process lifecycle.
use super::common::*;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileExt, MetadataExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
const PATTERNS: [&str; 3] = ["repeated", "random-shared", "random-unique"];
const ARMS: [&str; 7] = [
    "firecracker",
    "docker-rootless",
    "pvisor-off",
    "pvisor-ksm",
    "pvisor-physical",
    "pvisor-compressed",
    "pvisor-local",
];
const LIBRARIES: [(&str, &str, &str, Option<&str>); 7] = [
    ("firecracker-block", "firecracker", "block", None),
    ("docker-rootless-library", "docker-rootless", "rootfs", None),
    ("pvisor-rootfs-off", "pvisor-off", "rootfs", None),
    ("pvisor-rootfs-ksm", "pvisor-ksm", "rootfs", None),
    (
        "pvisor-rootfs-physical-no-hints",
        "pvisor-physical",
        "rootfs",
        Some("0"),
    ),
    (
        "pvisor-rootfs-physical-hints",
        "pvisor-physical",
        "rootfs",
        Some("1"),
    ),
    ("pvisor-dax", "pvisor-off", "dax", None),
];
const TUNABLES: [&str; 8] = [
    "run",
    "pages_to_scan",
    "sleep_millisecs",
    "smart_scan",
    "advisor_mode",
    "merge_across_nodes",
    "use_zero_pages",
    "max_page_sharing",
];

fn numbers(text: &str) -> BTreeMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            Some((
                p.next()?.trim_end_matches(':').into(),
                p.next()?.parse().ok()?,
            ))
        })
        .collect()
}
fn now() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) },
        0
    );
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
fn state() -> Result<Value> {
    let root = Path::new("/sys/kernel/mm/ksm");
    let mut files = BTreeMap::new();
    for name in TUNABLES {
        files.insert(
            name,
            fs::read_to_string(root.join(name))?.trim().to_string(),
        );
    }
    let mut ksmd = None;
    for e in fs::read_dir("/proc")? {
        let e = e?;
        if e.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        if fs::read_to_string(e.path().join("comm")).is_ok_and(|s| s.trim() == "ksmd") {
            let fields = stat(&e.path())?;
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
            ksmd = Some(
                json!({"pid":e.file_name().to_string_lossy(),"starttime":fields[19],
                "cpu_seconds":(fields[11].parse::<u64>()?+fields[12].parse::<u64>()?) as f64/ticks}),
            );
        }
    }
    Ok(json!({"files":files,"ksmd":ksmd.context("ksmd CPU unavailable")?}))
}
fn default_ksm(state: &Value) -> Result<()> {
    for (k, v) in [
        ("run", "1"),
        ("pages_to_scan", "100"),
        ("sleep_millisecs", "20"),
        ("smart_scan", "1"),
        ("merge_across_nodes", "1"),
        ("use_zero_pages", "0"),
        ("max_page_sharing", "256"),
    ] {
        ensure!(
            state["files"][k] == v,
            "administrator must configure default KSM: {k}={v}; coordinator never writes sysfs"
        );
    }
    ensure!(
        state["files"]["advisor_mode"]
            .as_str()
            .unwrap_or("")
            .contains("[none]"),
        "KSM advisor must be none"
    );
    Ok(())
}
fn stable(a: &Value, b: &Value) -> Result<()> {
    ensure!(
        a["files"] == b["files"]
            && a["ksmd"]["pid"] == b["ksmd"]["pid"]
            && a["ksmd"]["starttime"] == b["ksmd"]["starttime"],
        "KSM settings/identity changed"
    );
    Ok(())
}
fn stat(proc: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(proc.join("stat"))?;
    Ok(text
        .rsplit_once(')')
        .context("invalid proc stat")?
        .1
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}
fn group_pids(group: &Path) -> Result<Vec<u32>> {
    fn walk(group: &Path, pids: &mut BTreeSet<u32>) -> Result<()> {
        if let Ok(text) = fs::read_to_string(group.join("cgroup.procs")) {
            for word in text.split_whitespace() {
                pids.insert(word.parse()?);
            }
        }
        for entry in fs::read_dir(group)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), pids)?
            }
        }
        Ok(())
    }
    let mut pids = BTreeSet::new();
    walk(group, &mut pids)?;
    Ok(pids.into_iter().collect())
}
fn own_group() -> Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let relative = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .context("cgroup v2 required")?;
    Ok(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}
fn belongs(pid: u32, group: &Path) -> Result<bool> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let prefix = group.strip_prefix("/sys/fs/cgroup")?.display().to_string();
    Ok(text.lines().any(|l| {
        l.strip_prefix("0::")
            .is_some_and(|p| p == format!("/{prefix}") || p.starts_with(&format!("/{prefix}/")))
    }))
}
fn affinity(pid: u32) -> Result<Vec<usize>> {
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::sched_getaffinity(pid as i32, std::mem::size_of_val(&mask), &mut mask) }
            == 0,
        "affinity unavailable"
    );
    Ok((0..libc::CPU_SETSIZE as usize)
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &mask) })
        .collect())
}
fn counters(group: &Path) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for key in [
        "memory.current",
        "memory.peak",
        "memory.stat",
        "memory.events",
        "memory.pressure",
        "memory.swap.current",
        "memory.max",
        "memory.swap.max",
        "cpu.stat",
        "cpu.max",
        "cpu.pressure",
        "pids.max",
        "pids.events",
        "pids.current",
    ] {
        out.insert(
            key.into(),
            json!(fs::read_to_string(group.join(key))?.trim()),
        );
    }
    let c = Value::Object(out);
    validate_budget(&c)?;
    Ok(c)
}
fn validate_budget(c: &Value) -> Result<()> {
    validate_memory_budget(c)?;
    ensure!(c["pids.max"] == "256", "task budget mismatch");
    ensure!(
        numbers(c["pids.events"].as_str().context("missing pids events")?)
            .get("max")
            .copied()
            .unwrap_or(0)
            == 0,
        "task budget exhausted"
    );
    Ok(())
}
fn validate_memory_budget(c: &Value) -> Result<()> {
    let quota = c["cpu.max"]
        .as_str()
        .context("missing cpu quota")?
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        quota.len() == 2 && quota[1] > 0 && quota[0] == 4 * quota[1],
        "wrong CPU quota"
    );
    ensure!(
        c["memory.max"] == "2147483648"
            && c["memory.swap.max"] == "0"
            && c["memory.swap.current"] == "0",
        "resource budget mismatch"
    );
    let events = numbers(
        c["memory.events"]
            .as_str()
            .context("missing memory events")?,
    );
    ensure!(
        ["oom", "oom_kill", "max"]
            .iter()
            .all(|k| events.get(*k).copied().unwrap_or(0) == 0),
        "memory budget event"
    );
    Ok(())
}
fn mapping_evidence(proc: &Path) -> Result<Value> {
    let mut mergeable = 0;
    let mut pool = 0;
    let mut ksm = 0;
    let mut size = 0;
    let path = proc.join("smaps");
    let file = BufReader::new(File::open(path)?);
    for line in file.lines() {
        let line = line?;
        let words = line.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            continue;
        }
        if let Some((low, high)) = words[0].split_once('-') {
            if let (Ok(low), Ok(high)) =
                (u64::from_str_radix(low, 16), u64::from_str_radix(high, 16))
            {
                size = high - low;
                if line.contains("memfd:pvisor-shared-ram") {
                    pool += size;
                }
                continue;
            }
        }
        if words[0] == "VmFlags:" && words.contains(&"mg") {
            mergeable += size;
        }
        if words[0] == "KSM:" {
            ksm += words[1].parse::<u64>()? * 1024;
        }
    }
    Ok(json!({"mergeable_bytes":mergeable,"pool_mapping_bytes":pool,"ksm_bytes":ksm}))
}
fn snapshot(group: &Path, name: &str, guests: &[u32], full: bool) -> Result<Value> {
    let mut processes = Vec::new();
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    for pid in group_pids(group)? {
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let row = (|| -> Result<Value> {
            let start = stat(&proc)?;
            let rollup = fs::read_to_string(proc.join("smaps_rollup"))?;
            ensure!(
                affinity(pid)? == vec![0, 1, 2, 3] && belongs(pid, group)?,
                "process placement escaped: {pid}"
            );
            let mut row = json!({"pid":pid,"starttime":start[19],"comm":fs::read_to_string(proc.join("comm"))?.trim(),
                "pss_bytes":numbers(&rollup).get("Pss").context("PSS missing")?*1024,"smaps_rollup":rollup,
                "cpu_user_seconds":start[11].parse::<u64>()? as f64/ticks,
                "cpu_system_seconds":start[12].parse::<u64>()? as f64/ticks,
                "cgroup":fs::read_to_string(proc.join("cgroup"))?,"cpu_affinity":[0,1,2,3]});
            if full {
                row["mapping_evidence"] = mapping_evidence(&proc)?;
            }
            ensure!(stat(&proc)?[19] == start[19], "process identity changed");
            Ok(row)
        })();
        // Never silently omit an unreadable live process from PSS.
        match row {
            Ok(v) => processes.push(v),
            Err(e) => {
                if proc.exists() {
                    return Err(e);
                } else {
                    processes.push(json!({"pid":pid,"exited_during_read":true}));
                }
            }
        }
    }
    for pid in guests {
        ensure!(
            processes
                .iter()
                .any(|p| p["pid"] == *pid && p["pss_bytes"].is_u64()),
            "live guest PSS missing"
        );
    }
    let mut storage = BTreeMap::new();
    if full {
        for process in &processes {
            let Some(pid) = process["pid"].as_u64() else {
                continue;
            };
            if let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) {
                for fd in fds {
                    let fd = fd?;
                    if fs::read_link(fd.path())
                        .is_ok_and(|p| p.to_string_lossy().contains("memfd:pvisor-"))
                    {
                        if let Ok(metadata) = fs::metadata(fd.path()) {
                            storage.insert(
                                (metadata.dev(), metadata.ino()),
                                (metadata.blocks() * 512, metadata.len()),
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(
        json!({"name":name,"monotonic_ns":now(),"cgroup":group,"counters":counters(group)?,
        "processes":processes,"guest_pids":guests,"ksm":if full {state()?}else{Value::Null},
        "pool_storage":{"unique_memfds":storage.len(),"allocated_file_bytes":storage.values().map(|v|v.0).sum::<u64>(),
            "logical_file_bytes":storage.values().map(|v|v.1).sum::<u64>(),
            "scope":"unique memfd filesystem blocks, not net memory savings; excludes index/kernel overhead"}}),
    )
}
fn foreign(owned: &BTreeSet<u32>) -> Result<Value> {
    let mut rows = Vec::new();
    for e in fs::read_dir("/proc")? {
        let e = e?;
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if owned.contains(&pid) {
            continue;
        }
        if let Ok(comm) = fs::read_to_string(e.path().join("comm")) {
            if [
                "pvisor",
                "vmm",
                "firecracker",
                "cargo",
                "rustc",
                "zig",
                "cargo-nextest",
                "pool_memory_cas",
                "qemu-system-x86",
                "qemu-system-aar",
                "vm_memory_scale",
            ]
            .contains(&comm.trim())
            {
                rows.push(json!({"pid":pid,"comm":comm.trim()}));
            }
        }
    }
    Ok(json!({"monotonic_ns":now(),"sessions":rows}))
}
fn quiet(root: &Path) -> Result<()> {
    let mut rows = Vec::new();
    for _ in 0..30 {
        let row = foreign(&BTreeSet::new())?;
        rows.push(row.clone());
        save(&root.join("admission.json"), &rows)?;
        ensure!(
            row["sessions"].as_array().unwrap().is_empty(),
            "foreign VM/build during quiet admission"
        );
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

/// Fixed payload oracle shared with the native fixture; ordinary tests only use
/// a page or two, never launch VMs or perform a measurement.
use super::fixture::page;
fn expected(pattern: &str, id: u64, percent: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for index in 0..16384 {
        h.update(page(pattern, id, index, percent));
    }
    format!("{:x}", h.finalize())
}
struct Guest {
    process: OwnedChild,
    id: u64,
    pid: u32,
    input: std::process::ChildStdin,
    lines: mpsc::Receiver<std::result::Result<String, String>>,
    stop_reader: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
}
fn start_reader<R: Read + AsRawFd + Send + 'static>(
    mut output: R,
    mut log: File,
) -> Result<(
    mpsc::Receiver<std::result::Result<String, String>>,
    Arc<AtomicBool>,
    thread::JoinHandle<()>,
)> {
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == 0,
        "cannot make guest stdout nonblocking"
    );
    let (tx, rx) = mpsc::sync_channel(1024);
    let stop = Arc::new(AtomicBool::new(false));
    let requested = stop.clone();
    let handle = thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut pending = Vec::new();
            let mut bytes = [0; 65536];
            while !requested.load(Ordering::SeqCst) {
                let mut fd = libc::pollfd {
                    fd: output.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut fd, 1, 50) };
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error.into());
                }
                if ready == 0 {
                    continue;
                }
                let count = match output.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) => return Err(e.into()),
                };
                log.write_all(&bytes[..count])?;
                log.flush()?;
                pending.extend_from_slice(&bytes[..count]);
                ensure!(pending.len() <= 1048576, "guest stdout line exceeds limit");
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let line = pending.drain(..=end).collect::<Vec<_>>();
                    if line.starts_with(b"F4K ") {
                        // Keep complete boot logs on disk, but do not fill a
                        // blocking protocol queue with diagnostic console lines.
                        tx.try_send(Ok(String::from_utf8(line)?.trim().to_string()))
                            .map_err(|e| anyhow::anyhow!("guest protocol queue: {e}"))?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = tx.try_send(Err(format!("{error:#}")));
        }
    });
    Ok((rx, stop, handle))
}
impl Guest {
    fn start(mut command: Command, id: u64, root: &Path) -> Result<Self> {
        let mut process = OwnedChild::spawn(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(File::create(root.join(format!("vm{id}.stderr")))?),
        )?;
        let input = process.0.stdin.take().context("stdin unavailable")?;
        let output = process.0.stdout.take().context("stdout unavailable")?;
        let (lines, stop_reader, reader) =
            start_reader(output, File::create(root.join(format!("vm{id}.stdout")))?)?;
        let pid = process.0.id();
        Ok(Self {
            process,
            id,
            pid,
            input,
            lines,
            stop_reader,
            reader: Some(reader),
        })
    }
    fn ack(&mut self, token: &str, percent: usize, hash: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?
                .map_err(anyhow::Error::msg)?;
            if !line.starts_with("F4K ") {
                continue;
            }
            ensure!(
                line.split_whitespace().collect::<Vec<_>>()
                    == vec![
                        "F4K",
                        token,
                        &self.id.to_string(),
                        &percent.to_string(),
                        hash
                    ],
                "unexpected/full-payload digest mismatch: {line}"
            );
            return Ok(json!({"token":token,"instance":self.id,"percent":percent,"sha256":hash}));
        }
    }
    fn command(&mut self, token: &str, op: &str, percent: usize, hash: &str) -> Result<Value> {
        writeln!(self.input, "{token} {op} {percent}")?;
        self.input.flush()?;
        self.ack(token, percent, hash)
    }
}
impl Drop for Guest {
    fn drop(&mut self) {
        self.stop_reader.store(true, Ordering::SeqCst);
        let _ = self.process.kill_and_wait();
        // Closing the receiver prevents a full log channel blocking the reader.
        let (_, empty) = mpsc::channel();
        let old = std::mem::replace(&mut self.lines, empty);
        drop(old);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn verify_receipt(root: &Path) -> Result<Value> {
    let receipt: Value = read_json(&root.join("receipt.json"))?;
    let hashes = receipt["hashes"]
        .as_object()
        .context("receipt hashes missing")?;
    ensure!(!hashes.is_empty(), "empty receipt");
    for (name, wanted) in hashes {
        let p = Path::new(name);
        ensure!(
            !p.is_absolute() && !p.components().any(|c| c == std::path::Component::ParentDir),
            "unsafe receipt path"
        );
        ensure!(
            hash(&root.join(p))? == wanted.as_str().context("invalid receipt hash")?,
            "input receipt mismatch: {name}"
        );
    }
    if let Some(wanted) = receipt["source_archive_sha256"].as_str() {
        ensure!(
            hash(&root.join("source.tar.gz"))? == wanted,
            "source archive changed"
        );
    }
    if let Some(runtime) = receipt["runtime_sha256"].as_object() {
        for (path, wanted) in runtime {
            ensure!(
                hash(Path::new(path))? == wanted.as_str().context("runtime hash")?,
                "Docker runtime changed: {path}"
            );
        }
    }
    for (tree, manifest) in [
        ("seed", "seed-manifest.json"),
        ("python-root", "python-root-manifest.json"),
    ] {
        if root.join(manifest).is_file() {
            verify_tree(&root.join(tree), &root.join(manifest))?;
        }
    }
    Ok(receipt)
}
fn verify_tree(root: &Path, manifest: &Path) -> Result<()> {
    let rows: Vec<Value> = read_json(manifest)?;
    let inventory = inventory(root)?;
    ensure!(
        rows.len() == inventory.len(),
        "frozen tree manifest item count changed"
    );
    let mut paths = BTreeSet::new();
    for row in rows {
        let name = row["path"].as_str().context("tree manifest path")?;
        let relative = Path::new(name);
        ensure!(
            !relative.is_absolute()
                && !relative
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                && paths.insert(name.to_string()),
            "unsafe/duplicate tree manifest path"
        );
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            row["mode"].as_u64() == Some((metadata.mode() & 0o7777) as u64),
            "tree mode changed: {name}"
        );
        if let Some(link) = row["symlink"].as_str() {
            ensure!(
                metadata.file_type().is_symlink()
                    && fs::read_link(&path)?.to_string_lossy() == link,
                "tree symlink changed: {name}"
            );
        } else if row["directory"] == true {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "tree directory changed"
            );
        } else {
            ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && row["bytes"].as_u64() == Some(metadata.len())
                    && row["sha256"] == hash(&path)?,
                "tree file changed: {name}"
            );
        }
    }
    Ok(())
}
fn worker_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PVISOR_MEMORY_WORKER") {
        return Ok(fs::canonicalize(p)?);
    }
    Ok(std::env::current_exe()?.with_file_name("pvisor-memory-worker"))
}
fn worker_command(cfg: &Value, id: u64, pool: Option<&Path>) -> Result<Command> {
    let input = Path::new(cfg["inputs"].as_str().context("inputs missing")?);
    let root = Path::new(cfg["root"].as_str().context("root missing")?);
    let arm = cfg["arm"].as_str().context("arm missing")?;
    let pattern = cfg["pattern"].as_str().context("pattern missing")?;
    let library = cfg["library_variant"]
        .as_str()
        .and_then(|name| LIBRARIES.iter().find(|v| v.0 == name));
    if arm == "firecracker" {
        let mode = if library.is_some() {
            "library.ext4"
        } else {
            "guest.ext4"
        };
        let boot = if library.is_some() {
            format!(
                "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/library-worker -- random-shared {id} block"
            )
        } else {
            format!(
                "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/worker -- {pattern} {id}"
            )
        };
        let file = root.join(format!("fc{id}.json"));
        save(
            &file,
            &json!({"boot-source":{"kernel_image_path":input.join("kernel"),"boot_args":boot},
            "drives":[{"drive_id":"rootfs","path_on_host":input.join(mode),"is_root_device":true,"is_read_only":true}],
            "machine-config":{"vcpu_count":1,"mem_size_mib":256,"smt":false}}),
        )?;
        let mut command = Command::new(input.join("firecracker"));
        command
            .args(["--enable-pci", "--no-api", "--config-file"])
            .arg(file);
        if input.join("firecracker-filter.bpf").exists() {
            command
                .arg("--seccomp-filter")
                .arg(input.join("firecracker-filter.bpf"));
        }
        return Ok(command);
    }
    ensure!(arm != "docker-rootless", "Docker requires private engine");
    let mut command = Command::new(input.join("probe"));
    command
        .arg("--rootfs")
        .arg(input.join(if library.is_some() {
            "python-root"
        } else {
            "root"
        }))
        .arg("--worker")
        .arg(input.join(if library.is_some() {
            "library-worker"
        } else {
            "worker"
        }))
        .arg("--firmware")
        .arg(input.join("firmware"))
        .arg("--stage")
        .arg(root.join(format!("vm{id}")))
        .args([
            "--mode",
            arm.strip_prefix("pvisor-").context("invalid pvisor arm")?,
            "--pattern",
            pattern,
            "--instance",
            &id.to_string(),
        ]);
    command.env_remove("PVISOR_MEMORY_FILE_HINTS");
    if let Some((_, _, mode, hints)) = library {
        if *mode == "dax" {
            command.arg("--libraries").arg(input.join("libraries"));
        } else {
            command.arg("--rootfs-library");
        }
        if let Some(hints) = hints {
            command.env("PVISOR_MEMORY_FILE_HINTS", hints);
        }
    }
    if let Some(socket) = pool {
        command.arg("--pool-socket").arg(socket);
    }
    Ok(command)
}
fn phase(root: &Path, result: &mut Value, group: &Path, name: &str, pids: &[u32]) -> Result<()> {
    let snap = snapshot(group, name, pids, true)?;
    save(&root.join(format!("{name}.json")), &snap)?;
    result["phases"].as_array_mut().unwrap().push(snap);
    save(&root.join("worker.json"), result)
}
fn ack(
    result: &mut Value,
    guest: &mut Guest,
    token: &str,
    op: Option<&str>,
    percent: usize,
    hashes: &Value,
) -> Result<()> {
    let hash = hashes[format!("{}:{percent}", guest.id)]
        .as_str()
        .context("expected hash missing")?;
    let row = if let Some(op) = op {
        guest.command(token, op, percent, hash)?
    } else {
        guest.ack(token, percent, hash)?
    };
    result["checks"].as_array_mut().unwrap().push(row);
    Ok(())
}
pub fn worker_main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let file = args.next().context("expected condition JSON")?;
    if file == "--slice-lease" {
        let unit = args.next().context("slice lease unit required")?;
        thread::sleep(Duration::from_secs(230));
        cmd(
            &[
                "systemctl",
                "--user",
                "stop",
                unit.to_str().context("unit UTF-8")?,
            ],
            15,
        )?;
        return Ok(());
    }
    let cfg: Value = read_json(Path::new(&file))?;
    internal(&cfg)
}
fn internal(cfg: &Value) -> Result<()> {
    if cfg["experiment"] == "scanner_compare" {
        return scanner_internal(cfg);
    }
    let root = Path::new(cfg["root"].as_str().context("root missing")?);
    let input = Path::new(cfg["inputs"].as_str().context("inputs missing")?);
    let arm = cfg["arm"].as_str().context("arm missing")?;
    let group = if let Some(p) = cfg["accounting_group"].as_str() {
        PathBuf::from(p)
    } else {
        own_group()?
    };
    let mut result =
        json!({"condition":cfg,"status":"failed","checks":[],"phases":[],"cleanup":[]});
    let operation = (|| {
        let initial = state()?;
        default_ksm(&initial)?;
        result["kernel_before"] = initial.clone();
        result["before"] = snapshot(&group, "before", &[], true)?;
        let mut engine = if arm == "docker-rootless" {
            Some(Engine::start(cfg)?)
        } else {
            None
        };
        let mut pool = None;
        let mut socket = None;
        if ["pvisor-physical", "pvisor-compressed"].contains(&arm) {
            let directory = root.join("pool");
            fs::create_dir(&directory)?;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
            save(
                &directory.join("config.json"),
                &json!({"mode":arm.trim_start_matches("pvisor-"),
                "max_bytes":536870912,"max_objects":32768,"max_connections":32,"max_references":32768}),
            )?;
            let log = File::create(root.join("pool.log"))?;
            let mut child = OwnedChild::spawn(
                Command::new(input.join("daemon"))
                    .arg("memory-pool")
                    .arg("--directory")
                    .arg(&directory)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone()?)
                    .stderr(log),
            )?;
            let path = directory.join("pool.sock");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !path.exists() {
                ensure!(
                    child.running()? && Instant::now() < deadline,
                    "pool readiness deadline"
                );
                thread::sleep(Duration::from_millis(20));
            }
            socket = Some(path);
            pool = Some(child);
        }
        let mut guests = Vec::new();
        for id in 1..=4 {
            let mut guest = if let Some(engine) = engine.as_mut() {
                engine.guest(cfg, id)?
            } else {
                Guest::start(worker_command(cfg, id, socket.as_deref())?, id, root)?
            };
            ack(&mut result, &mut guest, "ready", None, 0, &cfg["hashes"])?;
            guests.push(guest);
        }
        let pids = guests.iter().map(|g| g.pid).collect::<Vec<_>>();
        result["all_ready_ns"] = json!(now());
        let origin = Instant::now();
        for boundary in [0, 1, 2, 5, 10, 20, 40, 60] {
            thread::sleep(Duration::from_secs(boundary).saturating_sub(origin.elapsed()));
            phase(
                root,
                &mut result,
                &group,
                &format!("idle-{boundary}"),
                &pids,
            )?;
        }
        let start = now();
        for g in &mut guests {
            ack(&mut result, g, "readback", Some("read"), 0, &cfg["hashes"])?;
        }
        result["first_full_read_ms"] = json!((now() - start) as f64 / 1e6);
        phase(root, &mut result, &group, "readback", &pids)?;
        if cfg["library_variant"].is_string() {
            let start = now();
            let request =
                json!({"pids":pids,"pool_pid":pool.as_ref().map(|p|p.0.id()),"cgroup":group});
            save(&root.join("library-audit-request.json"), &request)?;
            let deadline = Instant::now() + Duration::from_secs(25);
            while !root.join("library-audit-response.json").exists() {
                ensure!(Instant::now() < deadline, "library audit deadline");
                thread::sleep(Duration::from_millis(20));
            }
            let response: Value = read_json(&root.join("library-audit-response.json"))?;
            ensure!(
                response["status"] == "passed",
                "library audit failed: {response}"
            );
            result["library_audit"] =
                json!({"started_ns":start,"finished_ns":now(),"response":response});
        }
        let mut states = [0; 4];
        for percent in [25, 100] {
            for index in 0..4 {
                let id = guests[index].id;
                ack(
                    &mut result,
                    &mut guests[index],
                    &format!("write-{percent}-{id}"),
                    Some("mutate"),
                    percent,
                    &cfg["hashes"],
                )?;
                states[index] = percent;
                for (i, g) in guests.iter_mut().enumerate() {
                    ack(
                        &mut result,
                        g,
                        &format!("peer-{percent}-{id}"),
                        Some("read"),
                        states[i],
                        &cfg["hashes"],
                    )?;
                }
            }
            if percent == 100 {
                result["post_write_origin_ns"] = json!(now());
            }
            phase(root, &mut result, &group, &format!("cow-{percent}"), &pids)?;
        }
        let origin = result["post_write_origin_ns"].as_u64().unwrap();
        for boundary in [1, 2, 5, 10, 20, 30] {
            thread::sleep(Duration::from_nanos(
                (origin + boundary * 1_000_000_000).saturating_sub(now()),
            ));
            phase(
                root,
                &mut result,
                &group,
                &format!("post-write-{boundary}"),
                &pids,
            )?;
        }
        while !guests.is_empty() {
            let mut g = guests.remove(0);
            let id = g.id;
            ack(
                &mut result,
                &mut g,
                &format!("exit-{id}"),
                Some("exit"),
                100,
                &cfg["hashes"],
            )?;
            if arm == "firecracker" {
                g.process.signal(libc::SIGTERM)?;
            }
            let status = g.process.wait(10)?;
            ensure!(
                arm == "firecracker" || status.success(),
                "guest failed at exit"
            );
            if let Some(engine) = engine.as_mut() {
                engine.validate_exit(id, status.code())?;
            }
            result["cleanup"]
                .as_array_mut()
                .unwrap()
                .push(json!({"pid":g.pid,"returncode":status.code(),"reaped":true}));
            for peer in &mut guests {
                ack(
                    &mut result,
                    peer,
                    &format!("survivor-{id}"),
                    Some("read"),
                    100,
                    &cfg["hashes"],
                )?;
            }
        }
        if let Some(mut p) = pool {
            p.signal(libc::SIGTERM)?;
            p.wait(10)?;
        }
        if let Some(engine) = engine.as_mut() {
            engine.stop()?;
        }
        result["after"] = snapshot(&group, "after", &[], true)?;
        let after = state()?;
        default_ksm(&after)?;
        stable(&initial, &after)?;
        result["kernel_after"] = after;
        result["status"] = json!("passed");
        Ok(())
    })();
    if let Err(ref e) = operation {
        result["error"] = json!(format!("{e:#}"));
    }
    save(&root.join("worker.json"), &result)?;
    operation
}

struct Engine {
    root: PathBuf,
    runtime: tempfile::TempDir,
    data: PathBuf,
    slice: String,
    process: Option<OwnedChild>,
    containers: BTreeMap<u64, String>,
    image: String,
}
impl Engine {
    fn call(&self, args: &[String]) -> Result<String> {
        let mut command = vec![
            "/usr/bin/docker".into(),
            format!(
                "--host=unix://{}",
                self.runtime.path().join("docker.sock").display()
            ),
        ];
        command.extend_from_slice(args);
        super::common::command(&command, 30)
    }
    fn start(cfg: &Value) -> Result<Self> {
        let root = PathBuf::from(cfg["root"].as_str().context("root missing")?);
        let inputs = Path::new(
            cfg["docker_inputs"]
                .as_str()
                .context("Docker frozen inputs required")?,
        );
        let receipt = verify_receipt(inputs)?;
        ensure!(
            receipt["pool_receipt_sha256"]
                == hash(&Path::new(cfg["inputs"].as_str().unwrap()).join("receipt.json"))?,
            "Docker and VM frozen source differ"
        );
        let runtime = tempfile::Builder::new()
            .prefix("pvdc-")
            .tempdir_in("/tmp")?;
        let slice = cfg["docker_slice"]
            .as_str()
            .context("owned Docker slice missing")?
            .to_string();
        let mut engine = Self {
            root: root.clone(),
            runtime,
            data: root.join("docker-data"),
            slice,
            process: None,
            containers: BTreeMap::new(),
            image: receipt["image_id"]
                .as_str()
                .context("image ID missing")?
                .into(),
        };
        let config = root.join("docker-daemon.json");
        save(&config, &json!({}))?;
        let mut command = Command::new("/usr/bin/dockerd-rootless.sh");
        command
            .arg(format!("--config-file={}", config.display()))
            .arg(format!("--data-root={}", engine.data.display()))
            .arg(format!(
                "--exec-root={}",
                engine.runtime.path().join("exec").display()
            ))
            .arg(format!(
                "--pidfile={}",
                engine.runtime.path().join("daemon.pid").display()
            ))
            .arg(format!(
                "--host=unix://{}",
                engine.runtime.path().join("docker.sock").display()
            ))
            .arg(format!("--cgroup-parent={}", engine.slice))
            .args([
                "--bridge=none",
                "--iptables=false",
                "--ip6tables=false",
                "--ip-forward=false",
                "--ip-masq=false",
                "--userland-proxy=false",
            ])
            .env(
                "DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR",
                engine.runtime.path().join("kit"),
            )
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS", "false")
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_NET", "none")
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_PORT_DRIVER", "none")
            .stdin(Stdio::null())
            .stdout(File::create(root.join("docker.stdout"))?)
            .stderr(File::create(root.join("docker.stderr"))?);
        save(
            &root.join("docker-launch.json"),
            &json!({"command":format!("{command:?}")}),
        )?;
        engine.process = Some(OwnedChild::spawn(&mut command)?);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(info) = engine.call(&["info".into(), "--format".into(), "{{json .}}".into()])
            {
                let info: Value = serde_json::from_str(&info)?;
                ensure!(
                    info["CgroupDriver"] == "systemd"
                        && info["CgroupVersion"] == "2"
                        && info["Driver"] == "overlayfs"
                        && info["SecurityOptions"]
                            .as_array()
                            .is_some_and(|a| a.contains(&json!("name=rootless"))),
                    "private Docker capabilities differ"
                );
                save(&root.join("docker-info.json"), &info)?;
                return Ok(engine);
            }
            ensure!(
                engine.process.as_ref().unwrap().running()? && Instant::now() < deadline,
                "private Docker readiness deadline"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn inspect(&self, cid: &str) -> Result<Value> {
        let rows: Value = serde_json::from_str(&self.call(&["inspect".into(), cid.into()])?)?;
        Ok(rows[0].clone())
    }
    fn guest(&mut self, cfg: &Value, id: u64) -> Result<Guest> {
        let library = cfg["library_variant"].is_string();
        let mut args = vec![
            "create".into(),
            "--name".into(),
            format!("memory-{id}"),
            format!("--cgroup-parent={}", self.slice),
            "--network=none".into(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--memory=256m".into(),
            "--memory-swap=256m".into(),
            "--cpus=1".into(),
            "--pids-limit=16".into(),
            "-i".into(),
            self.image.clone(),
            "/memory-init".into(),
        ];
        if library {
            args.extend([
                "/library-worker".into(),
                "random-shared".into(),
                id.to_string(),
                "rootfs".into(),
            ]);
        } else {
            args.extend([
                "/worker".into(),
                "--native".into(),
                cfg["pattern"].as_str().unwrap().into(),
                id.to_string(),
            ]);
        }
        let cid = self.call(&args)?.trim().to_string();
        ensure!(!cid.is_empty(), "empty container identity");
        self.containers.insert(id, cid.clone());
        let mut command = Command::new("/usr/bin/docker");
        command
            .arg(format!(
                "--host=unix://{}",
                self.runtime.path().join("docker.sock").display()
            ))
            .args(["start", "-ai", &cid]);
        let mut guest = Guest::start(command, id, &self.root)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let item = self.inspect(&cid)?;
            if item["State"]["Running"] == true {
                let pid = item["State"]["Pid"].as_u64().context("host PID missing")? as u32;
                ensure!(
                    Path::new(&format!("/proc/{pid}")).exists(),
                    "Docker host PID unavailable"
                );
                let host = &item["HostConfig"];
                ensure!(
                    item["Image"] == self.image
                        && host["CgroupParent"] == self.slice
                        && host["Memory"] == 268435456u64
                        && host["MemorySwap"] == 268435456u64
                        && host["NanoCpus"] == 1_000_000_000u64
                        && host["PidsLimit"] == 16
                        && host["ReadonlyRootfs"] == true
                        && host["NetworkMode"] == "none"
                        && host["CapDrop"] == json!(["ALL"]),
                    "installed container configuration differs"
                );
                let proc = PathBuf::from(format!("/proc/{pid}"));
                let placement = fs::read_to_string(proc.join("cgroup"))?;
                let relative = placement
                    .lines()
                    .find_map(|l| l.strip_prefix("0::"))
                    .context("container cgroup missing")?;
                let leaf = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
                let limits = [
                    "memory.max",
                    "memory.swap.max",
                    "memory.swap.current",
                    "pids.max",
                    "cpu.max",
                ]
                .into_iter()
                .map(|n| {
                    Ok((
                        n.to_string(),
                        fs::read_to_string(leaf.join(n))?.trim().to_string(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
                let cpu = limits["cpu.max"].split_whitespace().collect::<Vec<_>>();
                ensure!(
                    limits["memory.max"] == "268435456"
                        && limits["memory.swap.max"] == "0"
                        && limits["memory.swap.current"] == "0"
                        && limits["pids.max"] == "16"
                        && cpu.len() == 2
                        && cpu[0] == cpu[1],
                    "actual container limits differ"
                );
                save(
                    &self.root.join(format!("docker-{id}-created.json")),
                    &json!({"inspection":item,"limits":limits}),
                )?;
                guest.pid = pid;
                return Ok(guest);
            }
            ensure!(
                Instant::now() < deadline && guest.process.running()?,
                "Docker container start deadline"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn validate_exit(&self, id: u64, code: Option<i32>) -> Result<()> {
        let item = self.inspect(&self.containers[&id])?;
        save(&self.root.join(format!("docker-{id}-exit.json")), &item)?;
        ensure!(
            item["State"]["Running"] == false
                && item["State"]["OOMKilled"] == false
                && item["State"]["ExitCode"] == 0
                && code == Some(0),
            "Docker exit/OOM/attach mismatch"
        );
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for cid in self.containers.values() {
            if let Err(e) = self.call(&["rm".into(), "-f".into(), cid.clone()]) {
                errors.push(format!("{e:#}"));
            }
        }
        self.containers.clear();
        if let Some(mut child) = self.process.take() {
            if let Err(e) = child.signal(libc::SIGTERM) {
                errors.push(format!("{e:#}"));
            }
            if let Err(e) = child.wait(15) {
                errors.push(format!("{e:#}"));
            }
        }
        save(
            &self.root.join("docker-cleanup.json"),
            &json!({"errors":errors}),
        )?;
        ensure!(errors.is_empty(), "private Docker cleanup failed");
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        if self.process.is_some() {
            let _ = self.stop();
        }
    }
}

#[derive(Clone)]
struct Mapping {
    low: u64,
    high: u64,
    offset: u64,
    device: u64,
    inode: u64,
    path: String,
}
fn mappings(pid: u32) -> Result<Vec<Mapping>> {
    let text = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    text.lines()
        .map(|l| {
            let mut fields = l.splitn(6, char::is_whitespace).filter(|s| !s.is_empty());
            // maps uses variable spacing before path; parse five fixed fields first.
            let words = l.split_whitespace().collect::<Vec<_>>();
            ensure!(words.len() >= 5, "invalid maps line");
            let (low, high) = words[0].split_once('-').context("maps range")?;
            let (major, minor) = words[3].split_once(':').context("maps device")?;
            let _ = &mut fields;
            let path = if words.len() > 5 {
                words[5..].join(" ")
            } else {
                String::new()
            };
            Ok(Mapping {
                low: u64::from_str_radix(low, 16)?,
                high: u64::from_str_radix(high, 16)?,
                offset: u64::from_str_radix(words[2], 16)?,
                device: libc::makedev(
                    u32::from_str_radix(major, 16)?,
                    u32::from_str_radix(minor, 16)?,
                ),
                inode: words[4].parse()?,
                path,
            })
        })
        .collect()
}
fn present(pid: u32, identity: &Mapping) -> Result<BTreeMap<u64, u64>> {
    let file = File::open(format!("/proc/{pid}/pagemap"))?;
    let mut out = BTreeMap::new();
    for map in mappings(pid)?.into_iter().filter(|m| {
        m.device == identity.device && m.inode == identity.inode && m.path == identity.path
    }) {
        for address in (map.low..map.high).step_by(4096) {
            let mut bytes = [0; 8];
            file.read_exact_at(&mut bytes, address / 4096 * 8)?;
            let bits = u64::from_le_bytes(bytes);
            if bits & ((1 << 63) | (1 << 61)) == ((1 << 63) | (1 << 61)) {
                out.insert(address, map.offset + address - map.low);
            }
        }
    }
    Ok(out)
}
fn observe_pages(pids: &[u32], backing: &Path, payload: &Path) -> Result<Value> {
    let data = fs::read(payload)?;
    ensure!(
        data.len() == 64 * 1024 * 1024,
        "library payload must be 64 MiB"
    );
    let mut expected = BTreeMap::new();
    for (index, page) in data.chunks_exact(4096).enumerate() {
        ensure!(
            expected.insert(hash_bytes(page), index).is_none(),
            "library target pages must be unique"
        );
    }
    let source = File::open(backing)?;
    let metadata = source.metadata()?;
    // Calibrate maps device/inode/path against this pinned descriptor; stat device
    // alone is not authoritative on OverlayFS. Mapping is not faulted by this step.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            source.as_raw_fd(),
            0,
        )
    };
    ensure!(address != libc::MAP_FAILED, "canonical mmap failed");
    let identity = mappings(std::process::id())?
        .into_iter()
        .find(|m| m.low <= address as u64 && m.high > address as u64);
    unsafe { libc::munmap(address, 4096) };
    let identity = identity.context("canonical mapping identity unavailable")?;
    ensure!(
        identity.inode == metadata.ino() && identity.offset + address as u64 - identity.low == 0,
        "ambiguous canonical mapping identity"
    );
    let starts = pids
        .iter()
        .map(|pid| {
            Ok((
                *pid,
                stat(&PathBuf::from(format!("/proc/{pid}")))?[19].clone(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let before = pids
        .iter()
        .map(|pid| Ok((*pid, present(*pid, &identity)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let offsets = before
        .values()
        .flat_map(|m| m.values().copied())
        .collect::<BTreeSet<_>>();
    let mut matched = BTreeMap::new();
    let mut bytes = [0; 4096];
    for offset in offsets {
        ensure!(
            offset % 4096 == 0 && offset < metadata.len(),
            "mapping outside canonical file"
        );
        if offset + 4096 > metadata.len() {
            continue;
        }
        source.read_exact_at(&mut bytes, offset)?;
        if let Some(index) = expected.get(&hash_bytes(&bytes)) {
            if bytes == data[index * 4096..(index + 1) * 4096] {
                matched.insert(offset, *index);
            }
        }
    }
    let mut rows = Vec::new();
    let mut common: Option<BTreeSet<(usize, u64)>> = None;
    for pid in pids {
        let after = present(*pid, &identity)?;
        ensure!(
            stat(&PathBuf::from(format!("/proc/{pid}")))?[19] == starts[pid],
            "library process identity changed"
        );
        let slots = before[pid]
            .iter()
            .filter_map(|(address, offset)| {
                if after.get(address) == Some(offset) {
                    matched.get(offset).map(|index| (*index, *offset))
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        common = Some(match common {
            None => slots.clone(),
            Some(c) => c.intersection(&slots).copied().collect(),
        });
        rows.push(json!({"pid":pid,"starttime":starts[pid],"payload_pages":slots.iter().map(|s|s.0).collect::<BTreeSet<_>>().len(),
            "payload_index_and_file_offset":slots}));
    }
    let common = common.unwrap_or_default();
    Ok(
        json!({"schema":"pvisor-library-file-sharing/v2","maps_device":identity.device,"inode":identity.inode,
        "mapping_path":identity.path,"payload_sha256":hash_bytes(&data),"target_pages":16384,"processes":rows,
        "shared_pages_all_processes":common.iter().map(|s|s.0).collect::<BTreeSet<_>>().len(),
        "shared_payload_index_and_file_offset":common,
        "scope":"stable present/file PTE and complete canonical bytes, no PFNs; observed sharing is not net savings"}),
    )
}
fn audit(root: &Path, cfg: &Value, group: &Path) -> Result<()> {
    let request: Value = read_json(&root.join("library-audit-request.json"))?;
    ensure!(
        request["cgroup"] == group.display().to_string(),
        "audit cgroup mismatch"
    );
    let pids = request["pids"]
        .as_array()
        .context("audit PID list missing")?
        .iter()
        .map(|v| v.as_u64().map(|v| v as u32).context("invalid PID"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        pids.len() == 4 && pids.iter().copied().collect::<BTreeSet<_>>().len() == 4,
        "four distinct library processes required"
    );
    for pid in &pids {
        ensure!(belongs(*pid, group)?, "audit process escaped group");
    }
    let variant = LIBRARIES
        .iter()
        .find(|v| Some(v.0) == cfg["library_variant"].as_str())
        .context("invalid library variant")?;
    let input = Path::new(cfg["inputs"].as_str().unwrap());
    let started = now();
    let mut settings = Vec::new();
    for (i, pid) in pids.iter().enumerate() {
        let env = fs::read(format!("/proc/{pid}/environ"))?;
        let hints = env
            .split(|b| *b == 0)
            .filter_map(|e| e.strip_prefix(b"PVISOR_MEMORY_FILE_HINTS="))
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect::<Vec<_>>();
        ensure!(
            hints == variant.3.map(|s| vec![s.to_string()]).unwrap_or_default(),
            "observed hint setting differs"
        );
        let log = fs::read_to_string(root.join(format!("vm{}.stdout", i + 1)))?;
        ensure!(
            log.lines()
                .any(|l| l.starts_with(&format!("LIBRARY_MODE {} statx_attributes ", variant.2))),
            "guest library path not attested"
        );
        settings.push(json!({"pid":pid,"file_hints":variant.3}));
    }
    let mut evidence = json!({"status":"passed","variant":variant.0,"observed_start_ns":started,
        "payload_sha256":hash(&input.join("payload"))?,"observed_hint_settings":settings,"sharing":null});
    if variant.1 == "pvisor-physical" || variant.2 == "dax" {
        let source = if variant.2 == "dax" {
            input.join("libraries/libpayload.so")
        } else {
            let pid = request["pool_pid"].as_u64().context("pool PID required")?;
            ensure!(belongs(pid as u32, group)?, "pool escaped audit group");
            let mut files = BTreeMap::new();
            for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
                let entry = entry?;
                if fs::read_link(entry.path())
                    .is_ok_and(|p| p.to_string_lossy().contains("memfd:pvisor-shared-ram"))
                {
                    let m = fs::metadata(entry.path())?;
                    files.insert((m.dev(), m.ino()), entry.path());
                }
            }
            ensure!(files.len() == 1, "one physical arena required");
            files.into_values().next().unwrap()
        };
        evidence["sharing"] = observe_pages(&pids, &source, &input.join("payload"))?;
        if variant.2 == "dax" {
            ensure!(
                evidence["sharing"]["shared_pages_all_processes"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0,
                "DAX target sharing absent"
            );
        }
    }
    if variant.1 == "docker-rootless" {
        let mut views = Vec::new();
        for pid in &pids {
            let source = PathBuf::from(format!("/proc/{pid}/root/libraries/libpayload.so"));
            ensure!(
                hash(&source)? == hash(&input.join("libraries/libpayload.so"))?,
                "Docker view bytes differ"
            );
            let before = fs::read(format!("/proc/{pid}/smaps"))?;
            let proof = observe_pages(&[*pid], &source, &input.join("payload"))?;
            ensure!(
                proof["shared_pages_all_processes"] == 16384,
                "Docker full target PTE proof missing"
            );
            let after = fs::read(format!("/proc/{pid}/smaps"))?;
            let before_path = root.join(format!("docker-library-{pid}-before.smaps"));
            let after_path = root.join(format!("docker-library-{pid}-after.smaps"));
            fs::write(&before_path, before)?;
            fs::write(&after_path, after)?;
            ensure!(
                hash(&source)? == hash(&input.join("libraries/libpayload.so"))?,
                "Docker view changed"
            );
            views.push(json!({"pid":pid,"target":proof,"smaps_sha256":[hash(&before_path)?,hash(&after_path)?]}));
        }
        evidence["docker_file_views"] =
            json!({"processes":views,"shared_target_pages_all_processes":null});
    }
    evidence["observed_end_ns"] = json!(now());
    save(&root.join("library-evidence.json"), &evidence)?;
    save(
        &root.join("library-audit-response.json"),
        &json!({"status":"passed","evidence_sha256":hash(&root.join("library-evidence.json"))?}),
    )
}

fn settle(unit: &str, root: &Path) -> Result<()> {
    let _ = cmd(&["systemctl", "--user", "stop", unit], 15);
    let state = cmd(
        &[
            "systemctl",
            "--user",
            "show",
            unit,
            "-p",
            "ActiveState",
            "-p",
            "SubState",
            "-p",
            "ControlGroup",
        ],
        5,
    )?;
    save(
        &root.join("cleanup.json"),
        &json!({"unit":unit,"state":state}),
    )?;
    ensure!(
        state
            .lines()
            .any(|l| l == "ActiveState=inactive" || l == "ActiveState=failed"),
        "owned unit remains active"
    );
    let group = state
        .lines()
        .find_map(|l| l.strip_prefix("ControlGroup="))
        .unwrap_or("");
    if !group.is_empty() {
        let path = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
        ensure!(
            !path.exists() || group_pids(&path)?.is_empty(),
            "owned cgroup still populated"
        );
    }
    Ok(())
}
fn scanner_internal(cfg: &Value) -> Result<()> {
    let input = Path::new(cfg["inputs"].as_str().context("scanner input")?);
    let root = Path::new(cfg["root"].as_str().context("scanner output")?);
    let output = root.join("w");
    let mut command = Command::new(input.join("probe"));
    command
        .arg("--rootfs")
        .arg(input.join("rootfs"))
        .arg("--firmware")
        .arg(input.join("firmware"))
        .arg("--output")
        .arg(&output);
    for (key, value) in cfg["scanner_condition"]
        .as_object()
        .context("scanner condition")?
    {
        command.arg(format!("--{}", key.replace('_', "-")));
        if let Some(s) = value.as_str() {
            command.arg(s);
        } else {
            command.arg(value.to_string());
        }
    }
    save(
        &root.join("probe-command.json"),
        &json!({"command":format!("{command:?}")}),
    )?;
    let mut child = OwnedChild::spawn(
        command
            .stdout(File::create(root.join("probe.stdout"))?)
            .stderr(File::create(root.join("probe.stderr"))?),
    )?;
    let result = (|| {
        ensure!(
            child.wait(180)?.success(),
            "scanner native SDK probe failed"
        );
        let raw: Value = read_json(&output.join("raw.json"))?;
        let result = json!({"status":"passed","condition":cfg,"raw":raw});
        validate_scanner(&result, cfg)?;
        Ok(result)
    })();
    match result {
        Ok(value) => save(&root.join("worker.json"), &value),
        Err(error) => {
            save(
                &root.join("worker.json"),
                &json!({"status":"failed","condition":cfg,"error":format!("{error:#}")}),
            )?;
            Err(error)
        }
    }
}
fn scanner_counter(accounting: &Value, key: &str) -> Result<String> {
    Ok(accounting["counters"][key]["raw"]
        .as_str()
        .context(format!("missing scanner accounting {key}"))?
        .trim()
        .to_string())
}
fn scanner_accounting(accounting: &Value, group: &Value, output: &Path) -> Result<u64> {
    ensure!(
        accounting["cgroup"] == *group,
        "scanner accounting escaped group"
    );
    let mut c = serde_json::Map::new();
    for key in [
        "cpu.max",
        "memory.max",
        "memory.swap.max",
        "memory.swap.current",
        "memory.events",
    ] {
        c.insert(key.into(), json!(scanner_counter(accounting, key)?));
    }
    // SDK v1 snapshots predate task-limit fields. The independent continuous
    // observer validates actual pids.max/events; never fabricate them in raw.
    validate_memory_budget(&Value::Object(c))?;
    ensure!(
        accounting["process_errors"] == json!([]),
        "scanner process inventory incomplete"
    );
    for key in ["memory.pressure", "cpu.pressure"] {
        ensure!(!scanner_counter(accounting, key)?.is_empty(), "missing PSI");
    }
    let mem = numbers(&scanner_counter(accounting, "memory.stat")?);
    ensure!(
        ["anon", "file", "kernel"]
            .iter()
            .all(|k| mem.contains_key(*k)),
        "missing memory classes"
    );
    for process in accounting["processes"]
        .as_array()
        .context("scanner process list")?
    {
        let smaps = &process["smaps"];
        let text = if let Some(raw) = smaps["raw"].as_str() {
            raw.to_string()
        } else {
            let file = smaps["file"].as_str().context("missing smaps evidence")?;
            ensure!(
                Path::new(file).file_name().and_then(|s| s.to_str()) == Some(file)
                    && smaps["capture"] == "streamed-synced-cache-discarded",
                "unsafe smaps sidecar"
            );
            let path = output
                .parent()
                .context("output parent")?
                .join("smaps")
                .join(file);
            ensure!(
                hash(&path)? == smaps["sha256"]
                    && fs::metadata(&path)?.len()
                        == smaps["bytes"].as_u64().context("smaps bytes")?,
                "smaps sidecar changed"
            );
            fs::read_to_string(path)?
        };
        let totals = if process["smaps_vma_totals_bytes"].is_object() {
            &process["smaps_vma_totals_bytes"]
        } else {
            &process["smaps_totals_bytes"]
        };
        for (key, value) in totals.as_object().context("smaps totals missing")? {
            let observed = text
                .lines()
                .filter_map(|line| {
                    let (name, rest) = line.split_once(':')?;
                    if name == key {
                        rest.split_whitespace().next()?.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
                .sum::<u64>()
                * 1024;
            ensure!(Some(observed) == value.as_u64(), "smaps totals mismatch");
        }
        let rollup = process["smaps_rollup"]["raw"]
            .as_str()
            .context("PSS rollup required")?;
        let observed = numbers(rollup);
        for (key, value) in process["smaps_totals_bytes"]
            .as_object()
            .context("rollup totals")?
        {
            ensure!(
                observed.get(key).map(|v| v * 1024) == value.as_u64(),
                "rollup total mismatch"
            );
        }
    }
    numbers(&scanner_counter(accounting, "cpu.stat")?)
        .get("usage_usec")
        .copied()
        .context("scanner CPU usage missing")
}
fn validate_scanner(wrapper: &Value, cfg: &Value) -> Result<()> {
    ensure!(
        wrapper["status"] == "passed" && wrapper["condition"] == *cfg,
        "scanner worker mismatch"
    );
    let raw = &wrapper["raw"];
    let c = &cfg["scanner_condition"];
    ensure!(
        raw["schema"] == "pvisor-memory-scale/v1" && raw["correctness"] == "passed",
        "scanner integrity failed"
    );
    for (key, value) in c.as_object().context("scanner condition")? {
        ensure!(
            raw["conditions"][key] == *value,
            "scanner wrong condition {key}"
        );
    }
    let input = Path::new(cfg["inputs"].as_str().unwrap());
    let output = Path::new(cfg["root"].as_str().unwrap()).join("w");
    for (key, value) in [
        ("rootfs", input.join("rootfs")),
        ("firmware", input.join("firmware")),
        ("output", output.clone()),
    ] {
        ensure!(
            raw["conditions"][key] == json!(value),
            "scanner wrong input {key}"
        );
    }
    ensure!(
        raw["source"]["binary_sha256"] == hash(&input.join("probe"))?,
        "scanner executable receipt mismatch"
    );
    ensure!(
        raw["conditions"]["private_baselines"] == false && raw["cleanup"]["all_reaped"] == true,
        "scanner private baseline/reap contract"
    );
    ensure!(
        raw["conditions"]["independent_inodes"] != true
            && raw["conditions"]["group_memory_max"]
                .as_u64()
                .unwrap_or(2147483648)
                == 2147483648,
        "scanner private inode/group budget condition mismatch"
    );
    for (key, value) in [
        ("memory_mib", 256),
        ("cpus", 1),
        ("payload_bytes", 67108864),
        ("page_bytes", 4096),
        ("max_live_vms", 4),
        ("deadline_seconds", 180),
    ] {
        ensure!(
            raw["profile"][key] == value,
            "scanner profile mismatch {key}"
        );
    }
    if c["mode"] == "pool" {
        ensure!(
            raw["pool_cleanup"]["reaped"] == true
                && raw["pool_daemon_sha256"] == hash(&input.join("pool-daemon"))?
                && raw["pool_observations"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty()),
            "scanner pool ownership/evidence missing"
        );
    }
    let phases = raw["phases"].as_array().context("scanner phases")?;
    ensure!(
        phases
            .iter()
            .map(|p| p["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            == vec!["ready", "cow25", "cow100", "after_exit"],
        "scanner phases missing/reordered"
    );
    let group = &raw["before"]["cgroup"];
    let mut previous = 0;
    let mut clock = 0.;
    let window = &raw["ksm_scan_window"];
    ensure!(
        window["seconds"] == 60 && window["deadline_is_not_product_failure"] == true,
        "scanner 60-second window missing"
    );
    let accounting = std::iter::once(&raw["before"])
        .chain([&window["before"], &window["after"]])
        .chain(phases.iter().map(|p| &p["accounting"]))
        .chain(std::iter::once(&raw["after"]));
    for a in accounting {
        let cpu = scanner_accounting(a, group, &output)?;
        ensure!(cpu >= previous, "scanner CPU regression");
        previous = cpu;
        ensure!(
            a["ksm"]["run"]["raw"]
                .as_str()
                .is_some_and(|s| s.trim() == "1"),
            "scanner KSM state changed"
        );
    }
    for phase in phases {
        let after_exit = phase["name"] == "after_exit";
        let ids = if after_exit {
            vec![2, 3, 4]
        } else {
            vec![1, 2, 3, 4]
        };
        ensure!(
            phase["instances"] == json!(ids)
                && phase["heartbeat_stable"] == true
                && phase["paused"] == true
                && phase["offloaded"] == false,
            "scanner simultaneous barrier incomplete"
        );
        let beats = phase["heartbeat"].as_array().context("heartbeat list")?;
        ensure!(
            beats.len() == ids.len() && beats.iter().all(|b| b.as_u64().is_some_and(|n| n > 0)),
            "scanner heartbeat missing"
        );
        let elapsed = phase["elapsed_ms"]
            .as_f64()
            .context("scanner phase clock")?;
        ensure!(elapsed >= clock, "scanner phase clock regression");
        clock = elapsed;
    }
    let checks = raw["checks"].as_array().context("scanner checks")?;
    ensure!(
        !checks.is_empty() && checks.iter().all(|c| c["passed"] == true),
        "scanner failed correctness check"
    );
    let by_name = |name: &str| {
        checks
            .iter()
            .filter(|c| c["name"] == name)
            .map(|c| &c["evidence"])
            .collect::<Vec<_>>()
    };
    for (name, count) in [
        ("group_budget", 1),
        ("cleanup_all_reaped", 1),
        ("one_vm_cancelled_and_reaped", 1),
        ("ready_full_digest", 4),
        ("cow_full_digest", 8),
        ("peer_write_isolation", 12),
        ("survivor_full_digest", 3),
        ("orderly_exit_digest", 3),
        ("resume_heartbeat_progress", 15),
    ] {
        ensure!(
            by_name(name).len() == count,
            "scanner missing/duplicate {name}"
        );
    }
    ensure!(
        by_name("one_vm_cancelled_and_reaped")[0]["instance"] == 1,
        "wrong cancellation order"
    );
    let mut oracles = BTreeMap::new();
    for oracle in raw["expected_digests"]
        .as_array()
        .context("expected digests")?
    {
        let id = oracle["instance"].as_u64().context("oracle instance")?;
        let percent = oracle["percent"].as_u64().context("oracle percent")?;
        let digest = oracle["digest"].as_str().context("oracle digest")?;
        ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid full digest"
        );
        ensure!(
            oracles.insert((id, percent), digest).is_none(),
            "duplicate oracle"
        );
    }
    let required = std::iter::once((0, 0))
        .chain((1..=4).flat_map(|id| [0, 25, 100].map(move |p| (id, p))))
        .collect::<BTreeSet<_>>();
    ensure!(
        oracles.keys().copied().collect::<BTreeSet<_>>() == required,
        "incomplete scanner oracle"
    );
    let verify_ack = |ack: &Value, token: String, op: &str, id: u64, percent: u64| -> Result<()> {
        ensure!(
            ack["token"] == token
                && ack["op"] == op
                && ack["instance"] == id
                && ack["percent"] == percent
                && ack["digest"] == oracles[&(id, percent)]
                && ack["heartbeat"].as_u64().is_some_and(|n| n > 0)
                && ack["read_ms"].as_f64().is_some_and(|v| v >= 0.),
            "scanner payload/heartbeat/read proof mismatch"
        );
        Ok(())
    };
    for (i, a) in by_name("ready_full_digest").into_iter().enumerate() {
        let id = i as u64 + 1;
        verify_ack(a, format!("prepare-{id}"), "prepare", id, 0)?;
    }
    for (i, a) in by_name("cow_full_digest").into_iter().enumerate() {
        let id = i as u64 % 4 + 1;
        let percent = if i < 4 { 25 } else { 100 };
        verify_ack(a, format!("cow{percent}-{id}"), "mutate", id, percent)?;
    }
    let mut expected_peers = Vec::new();
    for percent in [25, 100] {
        for index in 0..4 {
            for id in index + 2..=4 {
                expected_peers.push((
                    format!("isolation-{percent}-{index}-{id}"),
                    id,
                    if percent == 25 { 0 } else { 25 },
                ));
            }
        }
    }
    for (a, (token, id, percent)) in by_name("peer_write_isolation")
        .into_iter()
        .zip(expected_peers)
    {
        verify_ack(a, token, "read", id, percent)?;
    }
    for (name, token, op) in [
        ("survivor_full_digest", "survivor", "read"),
        ("orderly_exit_digest", "exit", "exit"),
    ] {
        for (i, a) in by_name(name).into_iter().enumerate() {
            let id = i as u64 + 2;
            verify_ack(a, format!("{token}-{id}"), op, id, 100)?;
        }
    }
    let mut progress = by_name("resume_heartbeat_progress").into_iter();
    for phase in phases {
        for (id, heartbeat) in phase["instances"]
            .as_array()
            .unwrap()
            .iter()
            .zip(phase["heartbeat"].as_array().unwrap())
        {
            let e = progress.next().context("missing strict progress")?;
            ensure!(
                e["instance"] == *id
                    && e["phase"] == phase["name"]
                    && e["before"] == *heartbeat
                    && e["after"].as_u64().context("progress after")?
                        > e["before"].as_u64().context("progress before")?,
                "strict heartbeat progress invalid"
            );
        }
    }
    let mut last_heartbeat = BTreeMap::new();
    for check in checks {
        let evidence = &check["evidence"];
        if check["name"] == "resume_heartbeat_progress" {
            let id = evidence["instance"].as_u64().context("progress instance")?;
            let before = evidence["before"].as_u64().context("progress before")?;
            ensure!(
                before >= *last_heartbeat.get(&id).unwrap_or(&0),
                "frozen scanner heartbeat regressed"
            );
            last_heartbeat.insert(id, evidence["after"].as_u64().context("progress after")?);
        } else if evidence["token"].is_string() {
            let id = evidence["instance"].as_u64().context("ack instance")?;
            let beat = evidence["heartbeat"].as_u64().context("ack heartbeat")?;
            ensure!(
                beat >= *last_heartbeat.get(&id).unwrap_or(&0),
                "scanner guest heartbeat regressed"
            );
            last_heartbeat.insert(id, beat);
        }
    }
    let guests = raw["guests"]
        .as_array()
        .context("scanner terminal guests")?;
    ensure!(guests.len() == 4, "scanner missing terminal guests");
    let mut identities = BTreeSet::new();
    for (i, g) in guests.iter().enumerate() {
        let r = &g["result"];
        let id = i + 1;
        ensure!(
            g["instance"] == id
                && r["failure"].is_null()
                && r["state"] == if id == 1 { "cancelled" } else { "completed" },
            "scanner terminal state mismatch"
        );
        if id > 1 {
            ensure!(r["exit_code"] == 0, "scanner guest nonzero exit");
        }
        ensure!(
            r["started_at_unix_ms"].as_u64().is_some_and(|t| t > 0)
                && r["finished_at_unix_ms"].as_u64() >= r["started_at_unix_ms"].as_u64(),
            "scanner timestamps invalid"
        );
        let identity = (
            r["run_id"].as_str().context("run ID")?,
            r["execution_id"].as_str().context("execution ID")?,
        );
        ensure!(
            !identity.0.is_empty() && !identity.1.is_empty() && identities.insert(identity),
            "duplicate scanner Run/Execution"
        );
    }
    if c["dedup"] == true {
        let processes = phases[0]["accounting"]["processes"]
            .as_array()
            .context("ready processes")?;
        let mut mergeable = 0;
        for p in processes {
            if p["smaps"]["raw"].as_str().is_some_and(|s| {
                s.lines()
                    .any(|l| l.starts_with("VmFlags:") && l.split_whitespace().any(|w| w == "mg"))
            }) {
                mergeable += 1;
            } else if let Some(file) = p["smaps"]["file"].as_str() {
                let text = fs::read_to_string(output.parent().unwrap().join("smaps").join(file))?;
                if text
                    .lines()
                    .any(|l| l.starts_with("VmFlags:") && l.split_whitespace().any(|w| w == "mg"))
                {
                    mergeable += 1;
                }
            }
        }
        ensure!(mergeable >= 4, "scanner four mergeable VM mappings missing");
        for g in guests {
            let stderr = g["result"]["output"]["stderr"]
                .as_str()
                .context("dedup advice log")?;
            let accepted = stderr
                .split("dedup advice installation: accepted_bytes=")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            ensure!(accepted > 0, "scanner accepted advice missing");
        }
    }
    Ok(())
}
fn scanner_round(cfg: &Config, root: &Path, index: u64) -> Result<f64> {
    let receipt: Value = read_json(&cfg.inputs.join("build-receipt.json"))?;
    for (name, key) in [
        ("probe", "example_sha256"),
        ("pool-daemon", "binary_sha256"),
        ("source.tar.gz", "source_archive_sha256"),
    ] {
        ensure!(
            hash(&cfg.inputs.join(name))? == receipt[key],
            "scanner frozen build receipt mismatch {name}"
        );
    }
    let mut cells = PATTERNS
        .into_iter()
        .flat_map(|p| ["off", "ksm", "adaptive"].map(move |a| (p, a)))
        .collect::<Vec<_>>();
    Rng(cfg.seed.wrapping_add(index)).shuffle(&mut cells);
    save(&root.join("order.json"), &cells)?;
    let initial = state()?;
    let mut rows = Vec::new();
    for (n, (pattern, arm)) in cells.into_iter().enumerate() {
        let mut condition = json!({"vms":4,"mode":if arm=="adaptive"{"pool"}else{"fresh"},"pattern":pattern,
            "dedup":arm=="ksm","seed":20261006,"settle_ms":500,"ksm_wait_seconds":60,"memory_mib":256,"cpus":1});
        if arm == "adaptive" {
            condition["pool_daemon"] = json!(cfg.inputs.join("pool-daemon"));
        }
        let before = state()?;
        stable(&initial, &before)?;
        let raw = execution(
            json!({"experiment":"scanner_compare","inputs":cfg.inputs,"root":root.join(n.to_string()),
            "arm":arm,"pattern":pattern,"scanner_condition":condition}),
        )?;
        let after = state()?;
        stable(&initial, &after)?;
        rows.push(json!({"arm":arm,"pattern":pattern,"raw":raw["raw"],
            "kernel_before":before,"kernel_after":after,
            "global_ksmd_cpu_seconds":after["ksmd"]["cpu_seconds"].as_f64().unwrap()-before["ksmd"]["cpu_seconds"].as_f64().unwrap()}));
        save(&root.join("samples.json"), &rows)?;
    }
    let parts = cfg.metric.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 5,
        "scanner metric PATTERN/REFERENCE/CANDIDATE/ready|cow25|cow100/charged|pss"
    );
    let value = |arm: &str| -> Result<f64> {
        let row = rows
            .iter()
            .find(|r| r["pattern"] == parts[0] && r["arm"] == arm)
            .context("scanner metric arm")?;
        let phase = row["raw"]["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == parts[3])
            .context("scanner metric phase")?;
        if parts[4] == "charged" {
            Ok(scanner_counter(&phase["accounting"], "memory.current")?.parse()?)
        } else {
            ensure!(parts[4] == "pss", "unknown scanner byte metric");
            Ok(phase["accounting"]["processes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["smaps_totals_bytes"]["Pss"].as_u64().unwrap_or(0) as f64)
                .sum())
        }
    };
    Ok(value(parts[1])? - value(parts[2])?)
}
fn validate_worker(raw: &Value, cfg: &Value) -> Result<()> {
    if cfg["experiment"] == "scanner_compare" {
        return validate_scanner(raw, cfg);
    }
    ensure!(
        raw["status"] == "passed" && raw["condition"] == *cfg,
        "worker failed/condition mismatch"
    );
    let phases = raw["phases"].as_array().context("phases missing")?;
    let wanted = [0, 1, 2, 5, 10, 20, 40, 60]
        .map(|n| format!("idle-{n}"))
        .into_iter()
        .chain(["readback".into(), "cow-25".into(), "cow-100".into()])
        .chain([1, 2, 5, 10, 20, 30].map(|n| format!("post-write-{n}")))
        .collect::<Vec<_>>();
    ensure!(
        phases
            .iter()
            .map(|p| p["name"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            == wanted,
        "missing/reordered memory phases"
    );
    ensure!(
        phases[7]["monotonic_ns"]
            .as_u64()
            .context("idle timestamp")?
            .checked_sub(
                raw["all_ready_ns"]
                    .as_u64()
                    .context("all ready timestamp")?
            )
            .unwrap_or(0)
            >= 60_000_000_000,
        "short idle window"
    );
    ensure!(
        phases.last().unwrap()["monotonic_ns"]
            .as_u64()
            .context("post write timestamp")?
            .checked_sub(
                raw["post_write_origin_ns"]
                    .as_u64()
                    .context("write origin")?
            )
            .unwrap_or(0)
            >= 30_000_000_000,
        "short post-write window"
    );
    let pids = phases[0]["guest_pids"]
        .as_array()
        .context("guest PIDs missing")?;
    ensure!(
        pids.len() == 4
            && pids
                .iter()
                .map(Value::to_string)
                .collect::<BTreeSet<_>>()
                .len()
                == 4,
        "four simultaneous processes required"
    );
    let mut cpu = 0;
    let mut clock = 0;
    for p in std::iter::once(&raw["before"])
        .chain(phases)
        .chain(std::iter::once(&raw["after"]))
    {
        validate_budget(&p["counters"])?;
        let current = numbers(p["counters"]["cpu.stat"].as_str().context("CPU stat")?)
            .get("usage_usec")
            .copied()
            .context("CPU usage missing")?;
        let time = p["monotonic_ns"].as_u64().context("phase timestamp")?;
        ensure!(current >= cpu && time >= clock, "nonmonotonic counters");
        cpu = current;
        clock = time;
        default_ksm(&p["ksm"])?;
        stable(&raw["kernel_before"], &p["ksm"])?;
        let processes = p["processes"]
            .as_array()
            .context("process inventory missing")?;
        let mut seen_pids = BTreeSet::new();
        for process in processes {
            ensure!(
                seen_pids.insert(process["pid"].as_u64().context("process PID")?),
                "duplicate process"
            );
            if process["exited_during_read"] == true {
                continue;
            }
            let rollup = process["smaps_rollup"]
                .as_str()
                .context("process PSS evidence missing")?;
            ensure!(
                numbers(rollup).get("Pss").map(|n| n * 1024) == process["pss_bytes"].as_u64(),
                "PSS evidence mismatch"
            );
            ensure!(
                process["cpu_affinity"] == json!([0, 1, 2, 3]),
                "process affinity escaped"
            );
            let group = p["cgroup"]
                .as_str()
                .context("phase cgroup")?
                .strip_prefix("/sys/fs/cgroup")
                .context("cgroup root")?;
            let placed = process["cgroup"]
                .as_str()
                .context("process cgroup")?
                .lines()
                .filter_map(|l| l.strip_prefix("0::"))
                .any(|path| {
                    path == group
                        || (cfg["arm"] == "docker-rootless"
                            && path.starts_with(&format!("{group}/")))
                });
            ensure!(placed, "process escaped accounting group");
        }
        if p["name"] != "before" && p["name"] != "after" {
            ensure!(
                &p["guest_pids"] == &Value::Array(pids.clone()),
                "missing live guest"
            );
            if cfg["arm"] == "docker-rootless" {
                ensure!(
                    ["dockerd", "containerd"]
                        .iter()
                        .all(|comm| processes.iter().any(|p| p["comm"] == *comm)),
                    "Docker runtime omitted from accounting"
                );
            }
            for pid in pids {
                let process = processes
                    .iter()
                    .find(|r| r["pid"] == *pid)
                    .context("live VM omitted")?;
                let mergeable = process["mapping_evidence"]["mergeable_bytes"]
                    .as_u64()
                    .context("mapping evidence")?;
                ensure!(
                    (cfg["arm"] == "pvisor-ksm") == (mergeable > 0),
                    "wrong KSM participation"
                );
            }
        }
    }
    let checks = raw["checks"].as_array().context("checks missing")?;
    let mut expected = BTreeMap::new();
    let mut states = [0; 4];
    for token in ["ready", "readback"] {
        for id in 1..=4 {
            expected.insert((token.to_string(), id), 0);
        }
    }
    for percent in [25, 100] {
        for id in 1..=4 {
            expected.insert((format!("write-{percent}-{id}"), id), percent);
            states[id - 1] = percent;
            for peer in 1..=4 {
                expected.insert((format!("peer-{percent}-{id}"), peer), states[peer - 1]);
            }
        }
    }
    for id in 1..=4 {
        expected.insert((format!("exit-{id}"), id), 100);
        for peer in id + 1..=4 {
            expected.insert((format!("survivor-{id}"), peer), 100);
        }
    }
    ensure!(
        checks.len() == expected.len(),
        "missing/duplicate oracle checks"
    );
    let mut seen = BTreeSet::new();
    for c in checks {
        let id = c["instance"].as_u64().context("check instance")? as usize;
        let token = c["token"].as_str().context("check token")?.to_string();
        ensure!(seen.insert((token.clone(), id)), "duplicate check");
        let percent = *expected.get(&(token, id)).context("unexpected check")?;
        ensure!(
            c["percent"] == percent && c["sha256"] == cfg["hashes"][format!("{id}:{percent}")],
            "payload oracle mismatch"
        );
    }
    let cleanup = raw["cleanup"].as_array().context("cleanup missing")?;
    ensure!(
        cleanup.len() == 4
            && pids.iter().all(|pid| cleanup
                .iter()
                .filter(|c| c["pid"] == *pid && c["reaped"] == true)
                .count()
                == 1),
        "native reap incomplete"
    );
    stable(&raw["kernel_before"], &raw["kernel_after"])?;
    if cfg["library_variant"].is_string() {
        validate_library(raw, cfg)?;
    }
    Ok(())
}
fn validate_library(raw: &Value, cfg: &Value) -> Result<()> {
    let root = Path::new(cfg["root"].as_str().context("library evidence root")?);
    let receipt = &raw["library_audit"];
    ensure!(
        receipt["response"]["status"] == "passed"
            && receipt["response"]["evidence_sha256"] == hash(&root.join("library-evidence.json"))?,
        "library evidence missing or changed"
    );
    let evidence: Value = read_json(&root.join("library-evidence.json"))?;
    ensure!(
        evidence["status"] == "passed"
            && evidence["variant"] == cfg["library_variant"]
            && evidence["payload_sha256"] == cfg["hashes"]["1:0"],
        "library evidence identity mismatch"
    );
    let phases = raw["phases"].as_array().context("library phases")?;
    let phase = |name: &str| {
        phases
            .iter()
            .find(|p| p["name"] == name)
            .context("library phase missing")
    };
    let times = [
        phase("readback")?["monotonic_ns"].as_u64(),
        receipt["started_ns"].as_u64(),
        evidence["observed_start_ns"].as_u64(),
        evidence["observed_end_ns"].as_u64(),
        receipt["finished_ns"].as_u64(),
        phase("cow-25")?["monotonic_ns"].as_u64(),
    ];
    ensure!(
        times.iter().all(Option::is_some) && times.windows(2).all(|p| p[0] <= p[1]),
        "library audit overlaps timed readback/private writes"
    );
    let variant = LIBRARIES
        .iter()
        .find(|v| Some(v.0) == cfg["library_variant"].as_str())
        .context("library variant")?;
    let pids = phase("idle-60")?["guest_pids"]
        .as_array()
        .context("library guests")?;
    let settings = evidence["observed_hint_settings"]
        .as_array()
        .context("library hint settings")?;
    ensure!(
        settings.len() == 4
            && pids.iter().all(|pid| settings
                .iter()
                .filter(|s| s["pid"] == *pid && s["file_hints"] == json!(variant.3))
                .count()
                == 1),
        "library observed hint setting mismatch"
    );
    let sharing = &evidence["sharing"];
    ensure!(
        sharing.is_object() == (variant.1 == "pvisor-physical" || variant.2 == "dax"),
        "library evidence scope mismatch"
    );
    if sharing.is_object() {
        ensure!(
            sharing["schema"] == "pvisor-library-file-sharing/v2"
                && sharing["target_pages"] == 16384
                && sharing["payload_sha256"] == cfg["hashes"]["1:0"],
            "library target mismatch"
        );
        let rows = sharing["processes"]
            .as_array()
            .context("library proof rows")?;
        ensure!(
            rows.len() == 4
                && pids
                    .iter()
                    .all(|pid| rows.iter().filter(|r| r["pid"] == *pid).count() == 1),
            "library proof ownership"
        );
        let mut common = None;
        for row in rows {
            let process = phase("idle-60")?["processes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["pid"] == row["pid"])
                .context("library process identity")?;
            ensure!(
                row["starttime"] == process["starttime"],
                "library process identity changed"
            );
            let slots = proof_slots(row)?;
            common = Some(match common {
                None => slots,
                Some(c) => {
                    let c: BTreeSet<(u64, u64)> = c;
                    c.intersection(&slots).copied().collect()
                }
            });
        }
        let common = common.unwrap();
        let retained = sharing["shared_payload_index_and_file_offset"]
            .as_array()
            .context("common slots")?
            .iter()
            .map(|v| {
                Ok((
                    v[0].as_u64().context("page index")?,
                    v[1].as_u64().context("file offset")?,
                ))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            retained == common
                && sharing["shared_pages_all_processes"]
                    == common.iter().map(|v| v.0).collect::<BTreeSet<_>>().len(),
            "library same-slot intersection mismatch"
        );
        if variant.2 == "dax" {
            ensure!(!common.is_empty(), "DAX target benefit missing");
        }
    }
    if variant.1 == "docker-rootless" {
        let views = &evidence["docker_file_views"];
        ensure!(
            views["shared_target_pages_all_processes"].is_null(),
            "Docker views must not be falsely equated"
        );
        let rows = views["processes"]
            .as_array()
            .context("Docker view proofs")?;
        ensure!(
            rows.len() == 4
                && pids
                    .iter()
                    .all(|pid| rows.iter().filter(|r| r["pid"] == *pid).count() == 1),
            "Docker library views incomplete"
        );
        for row in rows {
            let pid = row["pid"].as_u64().context("Docker proof PID")?;
            let proof = row["target"]["processes"]
                .as_array()
                .context("Docker page proof")?;
            ensure!(
                proof.len() == 1
                    && proof[0]["pid"] == pid
                    && proof_slots(&proof[0])?.len() == 16384,
                "Docker view PTE proof incomplete"
            );
            for (i, boundary) in ["before", "after"].into_iter().enumerate() {
                let path = root.join(format!("docker-library-{pid}-{boundary}.smaps"));
                ensure!(
                    hash(&path)? == row["smaps_sha256"][i],
                    "Docker exact-view smaps changed"
                );
            }
        }
    }
    Ok(())
}
fn proof_slots(row: &Value) -> Result<BTreeSet<(u64, u64)>> {
    let values = row["payload_index_and_file_offset"]
        .as_array()
        .context("target slots")?;
    let mut slots = BTreeSet::new();
    for value in values {
        let index = value[0].as_u64().context("target index")?;
        let offset = value[1].as_u64().context("target offset")?;
        ensure!(
            index < 16384 && offset % 4096 == 0 && slots.insert((index, offset)),
            "invalid/duplicate target slots"
        );
    }
    ensure!(
        row["payload_pages"] == slots.iter().map(|s| s.0).collect::<BTreeSet<_>>().len(),
        "target page count mismatch"
    );
    Ok(slots)
}
fn execution(mut cfg: Value) -> Result<Value> {
    let root = PathBuf::from(cfg["root"].as_str().context("root missing")?);
    fs::create_dir(&root)?;
    quiet(&root)?;
    let arm = cfg["arm"].as_str().unwrap().to_string();
    let unique = format!("pvisormemory{}{}", std::process::id(), now());
    let unit = if arm == "docker-rootless" {
        format!("{unique}.slice")
    } else {
        format!("{unique}.service")
    };
    let service = if arm == "docker-rootless" {
        format!("{unique}-worker.service")
    } else {
        unit.clone()
    };
    struct UnitGuard {
        unit: String,
    }
    impl Drop for UnitGuard {
        fn drop(&mut self) {
            let _ = cmd(&["systemctl", "--user", "stop", &self.unit], 15);
        }
    }
    let _unit_guard = UnitGuard { unit: unit.clone() };
    if arm == "docker-rootless" {
        let docker = Path::new(
            cfg["docker_inputs"]
                .as_str()
                .context("Docker inputs required")?,
        );
        verify_receipt(docker)?;
        command(
            &[
                "cp".into(),
                "-a".into(),
                "--reflink=always".into(),
                docker.join("seed").display().to_string(),
                root.join("docker-data").display().to_string(),
            ],
            60,
        )?;
        // Native anchor bounds the entire slice lifetime, not only the controller.
        let anchor = format!("{unique}.service");
        command(
            &[
                "systemd-run".into(),
                "--user".into(),
                "--quiet".into(),
                format!("--unit={anchor}"),
                format!("--slice={unit}"),
                "-p".into(),
                "RuntimeMaxSec=240".into(),
                worker_path()?.display().to_string(),
                "--slice-lease".into(),
                unit.clone(),
            ],
            15,
        )?;
        cmd(
            &[
                "systemctl",
                "--user",
                "set-property",
                "--runtime",
                &unit,
                "MemoryMax=2147483648",
                "MemorySwapMax=0",
                "CPUQuota=400%",
                "TasksMax=256",
            ],
            15,
        )?;
        cfg["docker_slice"] = json!(unit);
    }
    save(&root.join("config.json"), &cfg)?;
    let mut args = vec![
        "systemd-run".to_string(),
        "--user".into(),
        "--quiet".into(),
        "--wait".into(),
        "--pipe".into(),
        "--collect".into(),
        format!("--unit={service}"),
    ];
    if arm == "docker-rootless" {
        args.extend([
            format!("--slice={unit}"),
            "-p".into(),
            "Delegate=yes".into(),
        ]);
    }
    for property in [
        "MemoryMax=2147483648",
        "MemorySwapMax=0",
        "CPUQuota=400%",
        "CPUAffinity=0 1 2 3",
        "TasksMax=256",
        "RuntimeMaxSec=180",
        "TimeoutStopSec=10",
        "KillMode=control-group",
    ] {
        args.extend(["-p".into(), property.into()]);
    }
    args.extend([
        worker_path()?.display().to_string(),
        root.join("config.json").display().to_string(),
    ]);
    save(&root.join("command.json"), &args)?;
    let stop = Arc::new(AtomicBool::new(false));
    let watch_stop = stop.clone();
    let watch_root = root.clone();
    let watch_unit = unit.clone();
    let watch_cfg = cfg.clone();
    let watch = thread::spawn(move || -> Result<()> {
        let outcome = (|| -> Result<()> {
            let mut monitor = File::create(watch_root.join("monitor.jsonl"))?;
            let mut pss = File::create(watch_root.join("pss.jsonl"))?;
            let mut guards = File::create(watch_root.join("guard.jsonl"))?;
            let mut group = None;
            let mut next_pss = now();
            let mut next_guard = now();
            let mut count = 0;
            let mut audit_handle = None;
            let mut last_ksm = None;
            while !watch_stop.load(Ordering::SeqCst) {
                if group.is_none() {
                    if let Ok(relative) = cmd(
                        &[
                            "systemctl",
                            "--user",
                            "show",
                            &watch_unit,
                            "-p",
                            "ControlGroup",
                            "--value",
                        ],
                        5,
                    ) {
                        if !relative.trim().is_empty() {
                            group = Some(
                                Path::new("/sys/fs/cgroup")
                                    .join(relative.trim().trim_start_matches('/')),
                            );
                        }
                    }
                }
                if let Some(group) = group.as_ref().filter(|g| g.exists()) {
                    let row = json!({"monotonic_ns":now(),"counters":counters(group)?});
                    serde_json::to_writer(&mut monitor, &row)?;
                    monitor.write_all(b"\n")?;
                    monitor.flush()?;
                    count += 1;
                    if now() >= next_pss {
                        let row = snapshot(group, "external", &[], false)?;
                        serde_json::to_writer(&mut pss, &row)?;
                        pss.write_all(b"\n")?;
                        pss.flush()?;
                        next_pss = now() + 500_000_000;
                    }
                    if now() >= next_guard {
                        let row = foreign(&group_pids(group)?.into_iter().collect())?;
                        serde_json::to_writer(&mut guards, &row)?;
                        guards.write_all(b"\n")?;
                        guards.flush()?;
                        ensure!(
                            row["sessions"].as_array().unwrap().is_empty(),
                            "foreign VM/build during sampling"
                        );
                        let ksm = state()?;
                        if let Some(previous) = last_ksm.as_ref() {
                            stable(previous, &ksm)?;
                        }
                        last_ksm = Some(ksm);
                        next_guard = now() + 1_000_000_000;
                    }
                    if audit_handle.is_none()
                        && watch_root.join("library-audit-request.json").exists()
                    {
                        let mut audit_cfg = watch_cfg.clone();
                        audit_cfg["accounting_group"] = json!(group);
                        let root = watch_root.clone();
                        let group = group.clone();
                        // Page/PTE audit is independent of both 50 ms charged
                        // memory and 500 ms PSS observers. It cannot stall curves.
                        audit_handle = Some(thread::spawn(move || -> Result<()> {
                            let result = audit(&root, &audit_cfg, &group);
                            if let Err(ref error) = result {
                                save(
                                    &root.join("library-audit-response.json"),
                                    &json!({"status":"failed","error":format!("{error:#}")}),
                                )?;
                            }
                            result
                        }));
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
            if let Some(handle) = audit_handle {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("library audit panic"))??;
            }
            ensure!(count > 0, "external monitor observed no group");
            Ok(())
        })();
        if outcome.is_err() {
            // Fail closed while the worker is still running rather than wait
            // for its full idle window after an observer/placement violation.
            let _ = cmd(&["systemctl", "--user", "stop", &watch_unit], 15);
        }
        outcome
    });
    // Docker accounting group is known before launching worker. Parent slice,
    // not its worker service, includes dockerd and container systemd scopes.
    if arm == "docker-rootless" {
        let relative = cmd(
            &[
                "systemctl",
                "--user",
                "show",
                &unit,
                "-p",
                "ControlGroup",
                "--value",
            ],
            5,
        )?;
        ensure!(!relative.trim().is_empty(), "Docker slice missing");
        cfg["accounting_group"] =
            json!(Path::new("/sys/fs/cgroup").join(relative.trim().trim_start_matches('/')));
        save(&root.join("config.json"), &cfg)?;
    }
    let result = (|| {
        let mut child = OwnedChild::spawn(
            Command::new(&args[0])
                .args(&args[1..])
                .stdout(File::create(root.join("service.stdout"))?)
                .stderr(File::create(root.join("service.stderr"))?),
        )?;
        ensure!(child.wait(210)?.success(), "native memory worker failed");
        let raw: Value = read_json(&root.join("worker.json"))?;
        validate_worker(&raw, &cfg)?;
        Ok(raw)
    })();
    let cleanup = settle(&unit, &root);
    stop.store(true, Ordering::SeqCst);
    let watched = watch
        .join()
        .map_err(|_| anyhow::anyhow!("external observer panic"))?;
    // Cleanup must run even when worker or observer fails.
    cleanup?;
    watched?;
    let raw: Value = result?;
    save(
        &root.join("evidence-hashes.json"),
        &json!({"worker":hash(&root.join("worker.json"))?,
        "monitor":hash(&root.join("monitor.jsonl"))?,"pss":hash(&root.join("pss.jsonl"))?,
        "guard":hash(&root.join("guard.jsonl"))?}),
    )?;
    Ok(raw)
}
pub fn round(cfg: &Config, root: &Path, index: u64) -> Result<f64> {
    default_ksm(&state()?)?;
    if cfg.experiment == "scanner_compare" {
        return scanner_round(cfg, root, index);
    }
    let receipt = verify_receipt(&cfg.inputs)?;
    if cfg.experiment == "library_compare" {
        ensure!(
            receipt["worker_protocol"] == "native-library-v1",
            "library requires newly frozen native-library-v1 inputs; historical Python worker images are not silently reused"
        );
    }
    ensure!(
        cfg.docker_inputs.is_some(),
        "full seven-arm matrix requires private frozen Docker inputs"
    );
    let mut cells = Vec::new();
    if cfg.experiment == "library_compare" {
        for (name, arm, _, _) in LIBRARIES {
            cells.push(("random-shared", name, arm));
        }
    } else {
        for pattern in PATTERNS {
            for arm in ARMS {
                cells.push((pattern, arm, arm));
            }
        }
    }
    Rng(cfg.seed.wrapping_add(index)).shuffle(&mut cells);
    save(&root.join("order.json"), &cells)?;
    let mut hashes = BTreeMap::new();
    for pattern in PATTERNS {
        let mut map = serde_json::Map::new();
        for id in 1..=4 {
            for percent in [0, 25, 100] {
                map.insert(
                    format!("{id}:{percent}"),
                    json!(expected(pattern, id, percent)),
                );
            }
        }
        hashes.insert(pattern, Value::Object(map));
    }
    let mut rows = Vec::new();
    for (n, (pattern, variant, arm)) in cells.into_iter().enumerate() {
        verify_receipt(&cfg.inputs)?;
        let condition = json!({"schema":"pvisor-pool-curve-condition/v4","inputs":cfg.inputs,"docker_inputs":cfg.docker_inputs,
            "root":root.join(n.to_string()),"arm":arm,"pattern":pattern,"hashes":hashes[pattern],
            "library_variant":if cfg.experiment=="library_compare"{json!(variant)}else{Value::Null}});
        let raw = execution(condition)?;
        rows.push(json!({"arm":variant,"pattern":pattern,"raw":raw}));
        save(&root.join("samples.json"), &rows)?;
    }
    // Explicit metric chooses condition/phase, no synthetic sum across content
    // patterns and no substitution of guest capacity or command elapsed time.
    // Format: PATTERN/REFERENCE/CANDIDATE/PHASE/charged|pss
    let parts = cfg.metric.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 5,
        "metric must be PATTERN/REFERENCE/CANDIDATE/PHASE/charged|pss"
    );
    let value = |arm: &str| -> Result<f64> {
        let row = rows
            .iter()
            .find(|r| r["pattern"] == parts[0] && r["arm"] == arm)
            .context("metric arm not measured")?;
        let phase = row["raw"]["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == parts[3])
            .context("metric phase not measured")?;
        match parts[4] {
            "charged" => Ok(phase["counters"]["memory.current"]
                .as_str()
                .context("charged counter")?
                .parse::<f64>()?),
            "pss" => Ok(phase["processes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["pss_bytes"].as_u64().unwrap_or(0) as f64)
                .sum()),
            _ => bail!("memory metric must be charged or pss bytes"),
        }
    };
    Ok(value(parts[1])? - value(parts[2])?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oracle_page_matches_retained_generator() {
        assert_eq!(&page("repeated", 1, 0, 0)[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            page("random-shared", 1, 77, 0),
            page("random-shared", 4, 77, 0)
        );
        assert_ne!(
            page("random-unique", 1, 77, 0),
            page("random-unique", 4, 77, 0)
        );
        assert_ne!(
            page("random-shared", 1, 77, 25),
            page("random-shared", 4, 77, 25)
        );
    }
    #[test]
    fn missing_or_wrong_budgets_fail() {
        assert!(validate_budget(&json!({})).is_err());
        let mut c = json!({"cpu.max":"400000 100000","memory.max":"2147483648","memory.swap.max":"0",
            "memory.swap.current":"0","pids.max":"256","memory.events":"oom 0\noom_kill 0\nmax 0","pids.events":"max 0"});
        validate_budget(&c).unwrap();
        c["memory.swap.current"] = json!("1");
        assert!(validate_budget(&c).is_err());
    }
    #[test]
    fn missing_integrity_is_not_success() {
        assert!(validate_worker(&json!({"status":"passed","condition":{}}), &json!({})).is_err());
    }
    fn valid_worker() -> (Value, Value) {
        let cfg = json!({"arm":"pvisor-off","library_variant":null,"hashes":(1..=4).flat_map(|id|
            [0,25,100].map(move|p|(format!("{id}:{p}"),json!(format!("hash-{id}-{p}"))))).collect::<serde_json::Map<_,_>>()});
        let ksm = json!({"files":{"run":"1","pages_to_scan":"100","sleep_millisecs":"20","smart_scan":"1",
            "merge_across_nodes":"1","use_zero_pages":"0","max_page_sharing":"256","advisor_mode":"[none]"},
            "ksmd":{"pid":"99","starttime":"1"}});
        let snapshot = |name: &str, time: u64| {
            json!({"name":name,"monotonic_ns":time,"cgroup":"/sys/fs/cgroup/test","ksm":ksm,
                "counters":{"cpu.max":"400000 100000","cpu.stat":"usage_usec 1","memory.max":"2147483648",
                    "memory.swap.max":"0","memory.swap.current":"0","memory.events":"oom 0\noom_kill 0\nmax 0",
                    "pids.max":"256","pids.events":"max 0"},
                "guest_pids":[1,2,3,4],"processes":(1..=4).map(|pid|json!({"pid":pid,
                    "pss_bytes":1024,"smaps_rollup":"Pss: 1 kB","cpu_affinity":[0,1,2,3],
                    "cgroup":"0::/test\n","mapping_evidence":{"mergeable_bytes":0}})).collect::<Vec<_>>()})
        };
        let phases = [0, 1, 2, 5, 10, 20, 40, 60]
            .map(|n| snapshot(&format!("idle-{n}"), 1 + n * 1_000_000_000))
            .into_iter()
            .chain([
                snapshot("readback", 61_000_000_001),
                snapshot("cow-25", 62_000_000_001),
                snapshot("cow-100", 63_000_000_001),
            ])
            .chain([1, 2, 5, 10, 20, 30].map(|n| {
                snapshot(
                    &format!("post-write-{n}"),
                    63_000_000_001 + n * 1_000_000_000,
                )
            }))
            .collect::<Vec<_>>();
        let mut checks = Vec::new();
        let mut push = |token: String, id: usize, p: usize| {
            checks.push(json!({"token":token,"instance":id,
            "percent":p,"sha256":format!("hash-{id}-{p}")}))
        };
        for token in ["ready", "readback"] {
            for id in 1..=4 {
                push(token.into(), id, 0);
            }
        }
        let mut states = [0; 4];
        for p in [25, 100] {
            for id in 1..=4 {
                push(format!("write-{p}-{id}"), id, p);
                states[id - 1] = p;
                for peer in 1..=4 {
                    push(format!("peer-{p}-{id}"), peer, states[peer - 1]);
                }
            }
        }
        for id in 1..=4 {
            push(format!("exit-{id}"), id, 100);
            for peer in id + 1..=4 {
                push(format!("survivor-{id}"), peer, 100);
            }
        }
        let raw = json!({"status":"passed","condition":cfg,"kernel_before":ksm,"kernel_after":ksm,
            "all_ready_ns":1,"post_write_origin_ns":63_000_000_001u64,
            "before":snapshot("before",0),"after":snapshot("after",94_000_000_001),
            "phases":phases,"checks":checks,
            "cleanup":(1..=4).map(|pid|json!({"pid":pid,"reaped":true})).collect::<Vec<_>>()});
        (raw, cfg)
    }
    #[test]
    fn complete_phase_and_lifecycle_contract_accepts() {
        let (raw, cfg) = valid_worker();
        validate_worker(&raw, &cfg).unwrap();
    }
    #[test]
    fn incomplete_or_forged_phase_evidence_rejects() {
        let (raw, cfg) = valid_worker();
        for (pointer, value) in [
            ("/phases/7/monotonic_ns", json!(59_000_000_000u64)),
            ("/phases/16/monotonic_ns", json!(90_000_000_000u64)),
            ("/phases/0/guest_pids", json!([1, 2, 3])),
            ("/phases/0/counters/pids.max", json!("128")),
            ("/phases/0/counters/pids.events", json!("max 1")),
            (
                "/phases/0/counters/memory.events",
                json!("oom 1\noom_kill 0"),
            ),
            ("/phases/0/counters/memory.swap.current", json!("1")),
            ("/phases/0/processes/0/pss_bytes", json!(2048)),
            ("/phases/0/processes/0/cpu_affinity", json!([0, 1])),
            ("/phases/0/processes/0/cgroup", json!("0::/other")),
            (
                "/phases/0/processes/0/mapping_evidence/mergeable_bytes",
                json!(4096),
            ),
            ("/checks/0/sha256", json!("wrong")),
            ("/cleanup/0/reaped", json!(false)),
            ("/kernel_after/files/run", json!("0")),
        ] {
            let mut corrupt = raw.clone();
            *corrupt.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate_worker(&corrupt, &cfg).is_err(),
                "accepted {pointer}"
            );
        }
        let mut missing = raw.clone();
        missing["checks"].as_array_mut().unwrap().pop();
        assert!(validate_worker(&missing, &cfg).is_err());
        let mut duplicate = raw.clone();
        duplicate["checks"][1] = duplicate["checks"][0].clone();
        assert!(validate_worker(&duplicate, &cfg).is_err());
    }
    #[test]
    fn target_slots_require_unique_aligned_bounded_page_identities() {
        assert!(
            proof_slots(&json!({"payload_pages":1,"payload_index_and_file_offset":[[0,0]]}))
                .is_ok()
        );
        for slots in [
            json!([[0, 0], [0, 0]]),
            json!([[0, 1]]),
            json!([[16384, 0]]),
        ] {
            assert!(
                proof_slots(&json!({"payload_pages":1,"payload_index_and_file_offset":slots}))
                    .is_err()
            );
        }
    }
    #[test]
    fn native_library_command_does_not_invoke_python() {
        let cfg = json!({"inputs":"/frozen","root":"/trial","arm":"pvisor-physical","pattern":"random-shared",
            "library_variant":"pvisor-rootfs-physical-hints"});
        let command = worker_command(&cfg, 1, Some(Path::new("/pool.sock"))).unwrap();
        let argv = format!("{command:?}");
        assert_eq!(command.get_program(), "/frozen/probe");
        assert!(!command.get_args().any(|arg| arg == "/usr/bin/python3"
            || arg == "-B"
            || arg == "-u"
            || arg.to_string_lossy().ends_with(".py")));
        assert!(argv.contains("library-worker") && argv.contains("rootfs-library"));
    }
    #[test]
    fn stdout_reader_stops_when_an_external_writer_keeps_pipe_open() {
        use std::os::unix::net::UnixStream;
        let (input, _held_writer) = UnixStream::pair().unwrap();
        let (_lines, stop, reader) = start_reader(input, tempfile::tempfile().unwrap()).unwrap();
        let (done, completed) = mpsc::channel();
        stop.store(true, Ordering::SeqCst);
        thread::spawn(move || {
            reader.join().unwrap();
            let _ = done.send(());
        });
        completed
            .recv_timeout(Duration::from_secs(2))
            .expect("stdout reader hung on held-open pipe");
    }
    #[test]
    fn daemon_readiness_exit_can_be_cleaned_without_post_reap_signaling() {
        let root = tempfile::tempdir().unwrap();
        let child = OwnedChild::spawn(Command::new("sh").args(["-c", "exit 19"])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.running().unwrap() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let mut sentinel = OwnedChild::spawn(Command::new("sleep").arg("30")).unwrap();
        let mut engine = Engine {
            root: root.path().into(),
            runtime: tempfile::tempdir().unwrap(),
            data: root.path().join("seed"),
            slice: "owned-test.slice".into(),
            process: Some(child),
            containers: BTreeMap::new(),
            image: "unused".into(),
        };
        engine.stop().unwrap();
        assert!(sentinel.running().unwrap());
        sentinel.kill_and_wait().unwrap();
    }
}

//! Persistent private namespace owner; native control RPC is outside trial clocks.
use super::{
    process::{self, load, save, strings},
    proxy::Proxy,
    source::{self, Config},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

const CANARY: &[u8] = b"unchanged image startup input\n";
pub(super) struct Vm {
    pub output: PathBuf,
    child: Option<Child>,
    descriptor: Value,
    artifacts: Value,
}
impl Vm {
    pub(super) fn prepare(config: &Config) -> Result<Self> {
        let output = config.output.join("pvisor");
        fs::create_dir(&output)?;
        for dir in [
            "frozen",
            "frozen/bin",
            "frozen/firmware",
            "trials",
            "client-caches",
        ] {
            fs::create_dir(output.join(dir))?;
        }
        source::verify_store(&config.store)?;
        let mut artifacts = BTreeMap::new();
        let mut libraries = BTreeMap::new();
        for name in ["pvisor", "pvisor-daemon", "cache_probe"] {
            let dest = output.join("frozen/bin").join(name);
            let source = if name == "cache_probe" {
                config.binary_dir.join("examples/cache_probe")
            } else {
                config.binary_dir.join(name)
            };
            let hash = source::freeze(&source, &dest)?;
            artifacts.insert(dest.display().to_string(), hash);
            let raw = process::command(
                &strings(&[
                    "sh",
                    "-c",
                    "out=$(ldd \"$1\" 2>&1); code=$?; printf '%s\\n' \"$out\"; test \"$code\" = 0 || { test \"$code\" = 1 && case \"$out\" in *'not a dynamic executable'*|*'statically linked'*) true;; *) false;; esac; }",
                    "ldd-check",
                    dest.to_str().unwrap(),
                ]),
                &output.join(format!("ldd-{name}")),
                10,
            )?;
            ensure!(
                !std::str::from_utf8(&raw)?.contains("not found"),
                "unresolved runtime library"
            );
            // ldd output gives resolved absolute linked libraries; static CLI
            // needs no dynamic firmware, but this protocol uses explicit dynamic firmware.
            for token in std::str::from_utf8(&raw)?
                .split_whitespace()
                .filter(|s| s.starts_with('/'))
            {
                let path = Path::new(token);
                if path.is_file() {
                    let path = path.canonicalize()?;
                    libraries.insert(path.display().to_string(), source::digest(&path)?);
                }
            }
        }
        let firmware = output.join("frozen/firmware/libkrunfw.so.5");
        artifacts.insert(
            firmware.display().to_string(),
            source::freeze(&config.firmware, &firmware)?,
        );
        process::command(
            &strings(&[
                "cp",
                "-a",
                "--reflink=auto",
                config.store.to_str().unwrap(),
                output.join("service-store").to_str().unwrap(),
            ]),
            &output.join("copy-store"),
            600,
        )?;
        let rootfs = output
            .join("service-store/rootfs-v3/sha256")
            .join(source::hex(source::SOURCE)?);
        let tree = source::tree(&rootfs)?;
        ensure!(
            tree == source::tree(
                &config
                    .store
                    .join("rootfs-v3/sha256")
                    .join(source::hex(source::SOURCE)?)
            )?,
            "copied rootfs differs"
        );
        save(&output.join("rootfs-manifest.json"), &tree)?;
        let protocol = json!({"source":format!("docker.io/library/ubuntu@{}",source::SOURCE),"digest":source::SOURCE,
            "rootfs":rootfs,"cpus":config.cpus,"service_cpus":config.service_cpus,"memory_mib":2048,
            "timeout_seconds":90,"lifetime_seconds":86400,"cache_token":process::nonce()?,
            "artifacts":artifacts,"runtime_libraries":libraries,"workload_argv":source::workload(),
            "rootfs_manifest_sha256":source::digest(&output.join("rootfs-manifest.json"))?,
            "source_build_relationship":"preexisting artifacts; source-build correspondence unverified"});
        save(&output.join("protocol.json"), &protocol)?;
        fs::set_permissions(
            output.join("protocol.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        save(
            &output.join("containment-parent.json"),
            &json!({"namespaces":namespaces()?,"existing_pvisor":services()?}),
        )?;
        let executable = std::env::current_exe()?.canonicalize()?;
        let mut cmd = Command::new("unshare");
        cmd.args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child=KILL",
            "--mount-proc",
            "--propagation",
            "private",
        ])
        .arg(executable)
        .env("PVISOR_BENCH_IMAGE_WORKER", &output)
        .env("PVISOR_BENCH_ENTRY", "startup/image_compare.json")
        .stdout(File::create(output.join("worker.stdout"))?)
        .stderr(File::create(output.join("worker.stderr"))?)
        .process_group(0);
        let child = cmd.spawn()?;
        let pid = child.id();
        save(&output.join("supervisor.json"), &identity(pid)?)?;
        let mut this = Self {
            output,
            child: Some(child),
            descriptor: Value::Null,
            artifacts: protocol["artifacts"].clone(),
        };
        for name in [
            "protocol.json",
            "rootfs-manifest.json",
            "containment-parent.json",
        ] {
            let path = this.output.join(name);
            this.artifacts[path.display().to_string()] = source::digest(&path)?.into();
        }
        let deadline = Instant::now() + Duration::from_secs(100);
        loop {
            if this.output.join("worker.json").exists() {
                this.descriptor = load(&this.output.join("worker.json"))?;
                ensure!(
                    this.rpc(json!({"op":"status"}))?["valid"] == true,
                    "worker failed preflight"
                );
                for name in [
                    "containment-child.json",
                    "containment-chroot.json",
                    "cached-prepared.json",
                ] {
                    let path = this.output.join(name);
                    this.artifacts[path.display().to_string()] = source::digest(&path)?.into();
                }
                return Ok(this);
            }
            ensure!(
                this.child.as_mut().unwrap().try_wait()?.is_none(),
                "namespace worker exited; see worker.stderr"
            );
            ensure!(
                Instant::now() < deadline,
                "namespace worker startup exceeded 100 seconds"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub(super) fn pid(&self) -> u32 {
        self.child.as_ref().map(Child::id).unwrap_or(0)
    }
    fn rpc(&self, mut request: Value) -> Result<Value> {
        request["token"] = self.descriptor["token"].clone();
        let port = self.descriptor["port"]
            .as_u64()
            .context("missing worker port")?;
        let mut stream = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_secs(5),
        )?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        send(&mut stream, &request)?;
        receive(&mut stream)
    }
    pub(super) fn trial(&self, id: &str, lazy: bool, cache_key: &str, warm: bool) -> Result<Value> {
        let result = self.rpc(
            json!({"op":"trial","trial_id":id,"mode":if lazy{"lazy"}else{"prepared"},
            "cache_key":cache_key,"cache_state":if warm{"reuse"}else{"fresh"}}),
        )?;
        ensure!(result["valid"] == true, "VM trial failed: {result}");
        Ok(result)
    }
    pub(super) fn stop(&mut self) -> Result<()> {
        if self.child.is_none() {
            return Ok(());
        }
        let rpc = if self.descriptor.is_null() {
            Err(anyhow::anyhow!("worker did not become ready"))
        } else {
            self.rpc(json!({"op":"stop"}))
        };
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut stopped = false;
        let mut successful = false;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                successful = status.success();
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !stopped {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
        let result = (|| -> Result<Value> {
            rpc?;
            ensure!(stopped, "owned worker did not stop normally");
            ensure!(successful, "owned namespace worker teardown failed");
            let receipt = load(&self.output.join("worker-stop.json"))?;
            ensure!(
                receipt["valid"] == true,
                "worker teardown verification failed"
            );
            let child = load(&self.output.join("containment-child.json"))?;
            let own = child["namespaces"]["user"]
                .as_str()
                .context("namespace identity missing")?;
            let mut survivors = Vec::new();
            for entry in fs::read_dir("/proc")? {
                let path = entry?.path();
                if fs::read_link(path.join("ns/user"))
                    .ok()
                    .is_some_and(|p| p.to_string_lossy() == own)
                {
                    survivors.push(path);
                }
            }
            ensure!(survivors.is_empty(), "owned namespace survivors remain");
            let parent = load(&self.output.join("containment-parent.json"))?;
            let after = services()?;
            let before = parent["existing_pvisor"]
                .as_array()
                .context("service inventory")?;
            for service in before
                .iter()
                .filter(|s| s["user_namespace"] == parent["namespaces"]["user"])
            {
                ensure!(
                    after
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|s| s["pid"] == service["pid"]
                            && s["starttime"] == service["starttime"]
                            && s["command_sha256"] == service["command_sha256"]),
                    "preexisting same-userns service was not preserved"
                );
            }
            for (path, hash) in self.artifacts.as_object().context("artifact inventory")? {
                ensure!(
                    source::digest(Path::new(path))? == hash.as_str().unwrap(),
                    "frozen artifact changed"
                );
            }
            Ok(
                json!({"valid":true,"worker_receipt":receipt,"owned_namespace_survivors":survivors,
                "existing_pvisor_after":after,"existing_pvisor_preserved":true,
                "preservation_scope":"stable services proven to share caller user namespace; unknown namespaces observational"}),
            )
        })();
        save(
            &self.output.join("teardown.json"),
            &match &result {
                Ok(v) => v.clone(),
                Err(e) => json!({"valid":false,"error":format!("{e:#}")}),
            },
        )?;
        result.map(|_| ())
    }
}
impl Drop for Vm {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.stop();
        }
    }
}
fn identity(pid: u32) -> Result<Value> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = raw
        .rsplit_once(')')
        .context("invalid stat")?
        .1
        .split_whitespace()
        .collect();
    Ok(json!({"pid":pid,"starttime":fields[19].parse::<u64>()?}))
}
fn namespaces() -> Result<Value> {
    let mut result = json!({});
    for name in ["user", "mnt", "pid"] {
        result[name] = fs::read_link(format!("/proc/self/ns/{name}"))?
            .to_string_lossy()
            .into_owned()
            .into();
    }
    Ok(result)
}
fn services() -> Result<Value> {
    let mut rows = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let path = entry?.path();
        let Some(pid) = path
            .file_name()
            .and_then(|p| p.to_str())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let row = (|| -> Result<Option<Value>> {
            let command = fs::read(path.join("cmdline"))?;
            let argv: Vec<_> = command.split(|b| *b == 0).collect();
            let exe = argv
                .first()
                .map(|s| Path::new(std::str::from_utf8(s).unwrap_or("")).file_name())
                .flatten();
            let service = exe.is_some_and(|s| s == "pvisor")
                && argv.contains(&b"--pvisor-internal-host".as_slice())
                || exe.is_some_and(|s| s == "pvisor-daemon")
                    && argv.get(1) == Some(&b"serve".as_slice());
            if !service {
                return Ok(None);
            }
            let mut row = identity(pid)?;
            row["command_sha256"] = source::bytes_digest(&command).into();
            row["user_namespace"] = fs::read_link(path.join("ns/user"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
                .into();
            row["mnt_namespace"] = fs::read_link(path.join("ns/mnt"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
                .into();
            ensure!(
                identity(pid)?["starttime"] == row["starttime"],
                "service identity changed"
            );
            Ok(Some(row))
        })();
        if let Ok(Some(row)) = row {
            rows.push(row);
        }
    }
    Ok(json!(rows))
}
fn send(stream: &mut TcpStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn receive(stream: &mut TcpStream) -> Result<Value> {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    ensure!(
        length > 0 && length <= 8 * 1024 * 1024,
        "invalid control frame"
    );
    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn untimed(argv: &[&str], output: &Path, name: &str) -> Result<()> {
    process::command(&strings(argv), &output.join(name), 30)?;
    Ok(())
}
fn containment(output: &Path) -> Result<Value> {
    untimed(
        &[
            "mount",
            "-t",
            "tmpfs",
            "-o",
            "mode=1777,nosuid,nodev",
            "tmpfs",
            "/tmp",
        ],
        output,
        "mount-private-tmp",
    )?;
    let private = PathBuf::from(format!("/tmp/lazy-owned-root-{}", process::nonce()?));
    fs::create_dir(&private)?;
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(private.join(".old-root"))?;
    fs::create_dir(private.join("tmp"))?;
    fs::set_permissions(private.join("tmp"), fs::Permissions::from_mode(0o1777))?;
    let mut binds = Vec::new();
    let mut aliases = Vec::new();
    for name in [
        "/home", "/usr", "/etc", "/dev", "/proc", "/bin", "/sbin", "/lib", "/lib64", "/run",
        "/var", "/sys",
    ] {
        let path = Path::new(name);
        if name == "/lib64" && !path.exists() {
            continue;
        }
        let canonical = path.canonicalize()?;
        ensure!(
            canonical.is_dir() && canonical != Path::new("/") && !canonical.starts_with("/tmp"),
            "unsafe bind source"
        );
        if !binds.contains(&canonical) {
            binds.push(canonical);
        }
        if fs::symlink_metadata(path)?.is_symlink() {
            aliases.push((name.to_owned(), fs::read_link(path)?));
        }
    }
    binds.sort_by_key(|p| (p.components().count(), p.clone()));
    let mut minimal: Vec<PathBuf> = Vec::new();
    for path in binds {
        if !minimal.iter().any(|p| path.starts_with(p)) {
            minimal.push(path);
        }
    }
    ensure!(
        minimal.iter().any(|p| output.starts_with(p)),
        "evidence path cannot survive pivot_root"
    );
    let original = fs::metadata(output)?;
    let identity = (original.dev(), original.ino());
    for path in &minimal {
        fs::create_dir_all(private.join(path.strip_prefix("/")?))?;
    }
    for (name, target) in &aliases {
        std::os::unix::fs::symlink(target, private.join(name.trim_start_matches('/')))?;
    }
    let bin = private.join("tmp/lazy-binaries");
    fs::create_dir(&bin)?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    let mut hashes = BTreeMap::new();
    for name in ["pvisor", "pvisor-daemon", "cache_probe"] {
        let dest = bin.join(name);
        let hash = source::freeze(&output.join("frozen/bin").join(name), &dest)?;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o700))?;
        hashes.insert(name, hash);
    }
    let mut receipt = json!({"host_private_root":private,"binds":minimal,"symlinks":aliases,"binaries":hashes,
        "root_switch":"pivot_root","entered":false,"old_root_detached":false,"host_ownership_changes":"none"});
    save(&output.join("containment-chroot.json"), &receipt)?;
    untimed(
        &[
            "mount",
            "--bind",
            private.to_str().unwrap(),
            private.to_str().unwrap(),
        ],
        output,
        "bind-root",
    )?;
    for (index, path) in minimal.iter().enumerate() {
        untimed(
            &[
                "mount",
                "--rbind",
                path.to_str().unwrap(),
                private.join(path.strip_prefix("/")?).to_str().unwrap(),
            ],
            output,
            &format!("bind-{index}"),
        )?;
    }
    std::env::set_current_dir(&private)?;
    let new = std::ffi::CString::new(private.as_os_str().as_encoded_bytes())?;
    let old = std::ffi::CString::new(private.join(".old-root").as_os_str().as_encoded_bytes())?;
    ensure!(
        unsafe { libc::syscall(libc::SYS_pivot_root, new.as_ptr(), old.as_ptr()) } == 0,
        "pivot_root environment gap: {}",
        std::io::Error::last_os_error()
    );
    std::env::set_current_dir("/")?;
    untimed(&["umount", "-l", "/.old-root"], output, "detach-old-root")?;
    std::env::set_current_dir(output)?;
    let current = fs::metadata(output)?;
    ensure!(
        (current.dev(), current.ino()) == identity,
        "evidence identity changed after pivot"
    );
    for (name, hash) in &hashes {
        ensure!(
            source::digest(&Path::new("/tmp/lazy-binaries").join(name))? == *hash,
            "launch copy changed"
        );
    }
    receipt["entered"] = true.into();
    receipt["old_root_detached"] = true.into();
    receipt["evidence_path_unchanged"] = true.into();
    receipt["mountinfo"] = fs::read_to_string("/proc/self/mountinfo")?.into();
    save(&output.join("containment-chroot.json"), &receipt)?;
    Ok(receipt)
}
fn environment(protocol: &Value, output: &Path) -> BTreeMap<String, String> {
    [
        (
            "PVISOR_CACHE_TOKEN",
            protocol["cache_token"].as_str().unwrap().into(),
        ),
        ("PVISOR_STARTUP_TIMING", "0".into()),
        ("PVISOR_FS_PROFILE", "0".into()),
        ("PVISOR_LAZY_IMAGE_V2", "1".into()),
        ("PVISOR_LAZY_INDEX_PAGES", "1".into()),
        ("PVISOR_LAZY_BRIDGE_V2", "1".into()),
        ("TMPDIR", "/tmp".into()),
        ("XDG_RUNTIME_DIR", "/tmp".into()),
        (
            "XDG_CONFIG_HOME",
            output.join("service-config").display().to_string(),
        ),
        (
            "XDG_CACHE_HOME",
            output.join("service-client-cache").display().to_string(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}
fn trial(output: &Path, protocol: &Value, request: &Value, proxy: &Proxy) -> Result<Value> {
    let id = request["trial_id"].as_str().context("trial id")?;
    let key = request["cache_key"].as_str().context("cache key")?;
    for name in [id, key] {
        ensure!(
            !name.is_empty()
                && name.len() <= 96
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "unsafe trial/cache key"
        );
    }
    let lazy = request["mode"] == "lazy";
    ensure!(lazy || request["mode"] == "prepared", "unsupported mode");
    let warm = request["cache_state"] == "reuse";
    ensure!(
        warm || request["cache_state"] == "fresh",
        "unsupported cache state"
    );
    let folder = output.join("trials").join(id);
    fs::create_dir(&folder)?;
    let result = (|| -> Result<Value> {
        for (path, hash) in protocol["artifacts"]
            .as_object()
            .context("missing frozen artifacts")?
        {
            ensure!(
                source::digest(Path::new(path))?
                    == hash.as_str().context("invalid artifact hash")?,
                "frozen artifact changed before trial"
            );
        }
        let workspace = folder.join("workspace");
        fs::create_dir(&workspace)?;
        fs::write(workspace.join("startup-canary"), CANARY)?;
        let cache = output.join("client-caches").join(key);
        if lazy {
            if warm {
                ensure!(
                    cache.join("successful-cold.json").is_file(),
                    "reuse requires successful cold trial"
                );
            } else {
                fs::create_dir(&cache)?;
            }
        }
        let mut env = environment(protocol, output);
        for (k, path) in [
            ("XDG_CACHE_HOME", cache.clone()),
            ("XDG_CONFIG_HOME", folder.join("config")),
            ("PVISOR_RUN_HOME", folder.join("runs")),
            ("PVISOR_IMAGE_STORE", folder.join("store")),
        ] {
            env.insert(k.into(), path.display().to_string());
        }
        if lazy {
            env.insert("PVISOR_CACHE_SERVER".into(), proxy.address());
        }
        let cpus = protocol["cpus"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let rootfs = if lazy {
            format!("image={}", protocol["source"].as_str().unwrap())
        } else {
            protocol["rootfs"].as_str().unwrap().into()
        };
        let mut argv = strings(&[
            "taskset",
            "-c",
            &cpus,
            "/tmp/lazy-binaries/pvisor",
            "spawn",
            "--vm",
            "--rootfs",
            &rootfs,
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--gateway-mode",
            "off",
            "--stdio",
            "inherit",
            "--stage",
            folder.join("stage").to_str().unwrap(),
            "--cpu",
            "2",
            "--memory",
            "2048MiB",
            "--vm-library-dir",
            output.join("frozen/firmware").to_str().unwrap(),
            "--timeout",
            "90s",
            "--",
        ]);
        argv.extend(source::workload());
        save(&folder.join("command.json"), &argv)?;
        let cursor = proxy.cursor()?;
        let mut cmd = process::clean_command(&argv)?;
        cmd.current_dir(&workspace).envs(env);
        let observation = process::observe(
            &mut cmd,
            &folder.join("timed"),
            Some(source::MARKER),
            Duration::from_secs(90),
            None,
        )?;
        let traffic = proxy.observation(cursor, &folder)?;
        let requests = load(&folder.join("requests.json"))?;
        let opened = load(&output.join("cached-prepared.json"))?;
        let handle = opened["image_handle"]
            .as_str()
            .context("prepared handle missing")?;
        let mut at_ready = super::proxy::Traffic::default();
        for row in requests["rows"].as_array().context("missing requests")? {
            let request = &row["request"];
            match request["op"].as_str() {
                Some("read" | "stat" | "list") => ensure!(
                    request["digest"] == handle,
                    "cache read used a different immutable handle"
                ),
                Some("metadata" | "open") => ensure!(
                    request["handle"] == handle,
                    "cache metadata/open used a different immutable handle"
                ),
                Some("prepare") => ensure!(
                    row["response"]["image_handle"] == handle,
                    "cache prepare returned a different immutable handle"
                ),
                Some("ping") => {}
                _ => bail!("unsupported measured cache operation"),
            }
            if row["response_finished_ns"]
                .as_u64()
                .context("missing response timestamp")?
                <= observation
                    .timing
                    .marker_ns
                    .context("missing Ready timestamp")?
            {
                at_ready.content_bytes += row["content_bytes"].as_u64().unwrap();
                at_ready.metadata_bytes += row["metadata_bytes"].as_u64().unwrap();
                at_ready.response_bytes += row["response_bytes"].as_u64().unwrap();
            }
        }
        let mut bundles = Vec::new();
        find_bundles(&folder.join("stage"), &mut bundles)?;
        ensure!(bundles.len() == 1, "missing/duplicate VM Bundle");
        let bundle = load(&bundles[0])?;
        ensure!(
            bundle["run"]["state"] == "completed"
                && bundle["run"]["exit_code"] == 0
                && bundle["run"]["command"] == json!(source::workload())
                && bundle["run"]["executor"]["isolation"] == "virtual_machine"
                && bundle["safety"]["filesystem_changes_staged"] == true,
            "invalid staged VM Bundle"
        );
        ensure!(
            fs::read(workspace.join("startup-canary"))? == CANARY
                && fs::read_dir(&workspace)?.count() == 1,
            "host workspace/canary changed"
        );
        source::require_empty_upper(&folder.join("stage/upper"))?;
        if lazy {
            ensure!(
                (warm && traffic.content_bytes == 0) || (!warm && traffic.content_bytes > 0),
                "cold/hot cache content gate failed"
            );
        } else {
            ensure!(
                traffic.response_bytes == 0,
                "prepared VM used cache service"
            );
        }
        if lazy && !warm {
            save(
                &cache.join("successful-cold.json"),
                &json!({"trial_id":id,"source":protocol["source"]}),
            )?;
        }
        let result = json!({"valid":true,"trial_id":id,"mode":request["mode"],"cache_state":request["cache_state"],"cache_key":key,
            "ready_ns":observation.timing.ready_ns,"completion_ns":observation.timing.completion_ns,"timing":observation.timing,
            "traffic":traffic,"at_ready":at_ready,"bundle":bundles[0],"bundle_sha256":source::digest(&bundles[0])?,
            "timed_argv":argv,
            "stdout_sha256":source::bytes_digest(&observation.stdout),"requests_sha256":source::digest(&folder.join("requests.json"))?,
            "workload_argv":source::workload(),"actualMode":"virtual_machine","canary_unchanged":true,"stage_readonly_validated":true});
        Ok(result)
    })();
    let value = match result {
        Ok(value) => value,
        Err(error) => json!({"valid":false,"error":format!("{error:#}")}),
    };
    save(&folder.join("result.json"), &value)?;
    Ok(value)
}
fn find_bundles(path: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            find_bundles(&path, result)?;
        } else if path.file_name().is_some_and(|n| n == "run-bundle.json") {
            result.push(path);
        }
    }
    Ok(())
}
pub(super) fn worker(output: &Path) -> Result<()> {
    let protocol = load(&output.join("protocol.json"))?;
    let parent = load(&output.join("containment-parent.json"))?;
    let own = namespaces()?;
    for name in ["user", "mnt", "pid"] {
        ensure!(
            own[name] != parent["namespaces"][name],
            "namespace containment failed"
        );
    }
    let kvm = OpenKvm::new()?;
    save(
        &output.join("containment-child.json"),
        &json!({"namespaces":own,"kvm_api":12}),
    )?;
    let containment = containment(output)?;
    drop(kvm);
    let service_cpus: Vec<usize> = protocol["service_cpus"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap() as usize)
        .collect();
    process::pin(&service_cpus)?;
    let reservation = TcpListener::bind("127.0.0.1:0")?;
    let address = reservation.local_addr()?;
    drop(reservation);
    let argv = strings(&[
        "/tmp/lazy-binaries/pvisor-daemon",
        "serve",
        "--cache",
        "--cache-listen",
        &format!("tcp://{address}"),
        "--cache-image-store",
        output.join("service-store").to_str().unwrap(),
    ]);
    let mut command = process::clean_command(&argv)?;
    command
        .envs(environment(&protocol, output))
        .stdout(File::create(output.join("cache-service.stdout"))?)
        .stderr(File::create(output.join("cache-service.stderr"))?)
        .process_group(0);
    let mut service = command.spawn()?;
    let outcome = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            ensure!(
                service.try_wait()?.is_none() && Instant::now() < deadline,
                "cache service startup failed"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut proxy = Proxy::start_tcp(&address.to_string(), &output.join("proxy-frames"))?;
        let mut env = environment(&protocol, output);
        env.insert("PVISOR_CACHE_SERVER".into(), proxy.address());
        let mut cmd = process::clean_command(&strings(&[
            "/tmp/lazy-binaries/cache_probe",
            &json!({"op":"prepare", "image":protocol["source"], "architecture":"amd64"})
                .to_string(),
        ]))?;
        cmd.envs(env);
        let preparation = process::observe(
            &mut cmd,
            &output.join("cache-prepare"),
            None,
            Duration::from_secs(60),
            None,
        )?;
        let opened: Value = serde_json::from_slice(&preparation.stdout)?;
        ensure!(
            opened["status"] == "prepared"
                && opened["digest"] == source::SOURCE
                && opened["architecture"] == "amd64"
                && opened["image_handle"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
            "cache prepare returned wrong identity"
        );
        save(&output.join("cached-prepared.json"), &opened)?;
        proxy.observation(0, output)?;
        let control = TcpListener::bind("127.0.0.1:0")?;
        control.set_nonblocking(true)?;
        let token = process::nonce()?;
        save(
            &output.join("worker.json"),
            &json!({"port":control.local_addr()?.port(),"token":token,"namespaces":own}),
        )?;
        fs::set_permissions(
            output.join("worker.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        let expires = Instant::now() + Duration::from_secs(86400);
        let mut tainted = false;
        loop {
            ensure!(
                Instant::now() < expires,
                "namespace worker lifetime exceeded"
            );
            match control.accept() {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error.into()),
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    let request = receive(&mut stream)?;
                    ensure!(request["token"] == token, "control authentication failed");
                    match request["op"].as_str() {
                        Some("stop") => {
                            ensure!(
                                service.try_wait()?.is_none(),
                                "cache service exited before normal teardown"
                            );
                            send(&mut stream, &json!({"valid":true,"stopped":true}))?;
                            break;
                        }
                        Some("status") => send(
                            &mut stream,
                            &json!({"valid":!tainted&&service.try_wait()?.is_none()}),
                        )?,
                        Some("trial") => {
                            ensure!(
                                !tainted && service.try_wait()?.is_none(),
                                "worker unavailable/tainted"
                            );
                            let result = trial(output, &protocol, &request, &proxy)?;
                            tainted = result["valid"] != true;
                            send(&mut stream, &result)?;
                        }
                        _ => bail!("unsupported control request"),
                    }
                }
            }
        }
        proxy.stop()?;
        ensure!(!tainted, "cohort contains invalid VM trial");
        Ok(())
    })();
    unsafe {
        libc::kill(service.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let graceful = service.try_wait()?.is_some();
    if !graceful {
        let _ = service.kill();
    }
    let service_status = service.wait()?;
    let audit = (|| -> Result<()> {
        ensure!(graceful, "cache service needed forced termination");
        ensure!(
            source::digest(&output.join("rootfs-manifest.json"))?
                == protocol["rootfs_manifest_sha256"]
                    .as_str()
                    .context("missing rootfs identity")?,
            "rootfs manifest changed"
        );
        ensure!(
            source::tree(Path::new(protocol["rootfs"].as_str().unwrap()))?
                == load(&output.join("rootfs-manifest.json"))?,
            "rootfs changed"
        );
        for section in ["artifacts", "runtime_libraries"] {
            for (path, hash) in protocol[section].as_object().unwrap() {
                ensure!(
                    source::digest(Path::new(path))? == hash.as_str().unwrap(),
                    "artifact/library changed"
                );
            }
        }
        for (name, hash) in containment["binaries"].as_object().unwrap() {
            ensure!(
                source::digest(&Path::new("/tmp/lazy-binaries").join(name))?
                    == hash.as_str().unwrap(),
                "private launch binary changed"
            );
        }
        Ok(())
    })();
    save(
        &output.join("worker-stop.json"),
        &json!({"valid":outcome.is_ok()&&audit.is_ok(),"cache_service_exit":service_status.code(),
        "trial_error":outcome.as_ref().err().map(|e|format!("{e:#}")),"audit_error":audit.as_ref().err().map(|e|format!("{e:#}"))}),
    )?;
    outcome?;
    audit
}
struct OpenKvm {
    _file: File,
}
impl OpenKvm {
    fn new() -> Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")?;
        ensure!(
            unsafe { libc::ioctl(file.as_raw_fd(), 0xAE00) } == 12,
            "KVM API unavailable"
        );
        Ok(Self { _file: file })
    }
}

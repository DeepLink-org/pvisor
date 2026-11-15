//! Owned OverlayBD lab verification, private client reset and kernel-object audit.
use super::{
    process::{self, load, save, strings},
    proxy::Traffic,
    source,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) const CTR: &str = "/opt/overlaybd/snapshotter/ctr";
pub(super) const SOCKET: &str = "/lab/run/containerd.sock";
const BINARIES: [&str; 6] = [
    CTR,
    "/opt/overlaybd/snapshotter/overlaybd-snapshotter",
    "/opt/overlaybd/bin/overlaybd-tcmu",
    "/usr/local/bin/containerd",
    "/usr/local/bin/containerd-shim-runc-v2",
    "/usr/local/bin/runc",
];
pub(super) struct Cursor {
    dev: u64,
    ino: u64,
    size: u64,
}
pub(super) struct Lab {
    pub container: String,
    pub derived_reference: String,
    pub source_reference: String,
    pub source_digest: String,
    pub derived_digest: String,
    pub cpus: String,
    pub service_cpus: String,
    pub ctr: Vec<String>,
    pub docker: Vec<String>,
    pub baseline: Value,
    pub provenance: Value,
    pub manifest: Value,
    pub frozen: Value,
    pub path: PathBuf,
    pub prefix: String,
    evidence: PathBuf,
    index: u64,
    roles: BTreeMap<String, String>,
    _lock: File,
    artifacts: BTreeMap<String, String>,
}
impl Lab {
    pub(super) fn new(
        lab: &Path,
        evidence: &Path,
        cpus: &[usize],
        service_cpus: &[usize],
    ) -> Result<Self> {
        let allowed = process::affinity()?;
        ensure!(
            cpus.len() == 2
                && service_cpus.len() == 2
                && cpus[0] != cpus[1]
                && service_cpus[0] != service_cpus[1]
                && cpus.iter().chain(service_cpus).all(|c| allowed.contains(c))
                && !cpus.iter().any(|c| service_cpus.contains(c)),
            "require two distinct available application CPUs and disjoint service CPUs"
        );
        let lab = lab.canonicalize()?;
        ensure!(
            lab.components().any(|c| c.as_os_str() == ".data"),
            "owned lab must be under .data"
        );
        fs::create_dir(evidence)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lab.join("stage-comparison.lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another image experiment owns this lab"
        );
        let manifest = load(&lab.join("lab.json"))?;
        let source_record = load(&lab.join("source-image.json"))?;
        let id = manifest["id"].as_str().context("lab id missing")?;
        ensure!(
            id.len() == 12
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid lab id"
        );
        ensure!(
            Path::new(manifest["lab"].as_str().context("lab path missing")?).canonicalize()? == lab,
            "lab path mismatch"
        );
        let prefix = format!("/system.slice/pvisor-overlaybd-lab-{id}");
        ensure!(
            manifest["owned_cgroup_prefix"] == prefix && manifest["cgroup_namespace"] == "host",
            "owned cgroup contract mismatch"
        );
        let namespace = manifest["namespace"]
            .as_str()
            .context("private namespace missing")?;
        ensure!(
            !namespace.is_empty()
                && namespace
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "unsafe namespace"
        );
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&lab)? {
            let path = entry?.path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("smoke-")
                && path.join("frozen-images.json").is_file()
                && load(&path.join("result.json"))
                    .ok()
                    .is_some_and(|v| v["status"] == "passed")
            {
                candidates.push(path);
            }
        }
        candidates.sort();
        let smoke = candidates.pop().context("need a passed owned-lab smoke")?;
        let frozen = load(&smoke.join("frozen-images.json"))?;
        let port = manifest["port"].as_u64().context("registry port missing")?;
        let repository = source_record["repository"]
            .as_str()
            .context("source repository missing")?;
        let suffix = repository
            .strip_prefix(&format!("127.0.0.1:{port}/"))
            .context("registry must be owned loopback endpoint")?;
        ensure!(
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b)),
            "invalid repository"
        );
        ensure!(
            source_record["source_digest"] == source::SOURCE
                && frozen["source_digest"] == source::SOURCE,
            "source identity mismatch"
        );
        let source_reference = frozen["source_reference"]
            .as_str()
            .context("source reference missing")?
            .to_owned();
        let derived_reference = frozen["derived_reference"]
            .as_str()
            .context("derived reference missing")?
            .to_owned();
        let derived_digest = frozen["derived_digest"]
            .as_str()
            .context("derived digest missing")?
            .to_owned();
        source::hex(&derived_digest)?;
        ensure!(
            source_reference == format!("{repository}@{}", source::SOURCE)
                && derived_reference == format!("{repository}@{derived_digest}"),
            "frozen references mismatch"
        );
        let cpus = cpus
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let service_cpus = service_cpus
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut this = Self {
            container: manifest["container"]
                .as_str()
                .context("container missing")?
                .to_owned(),
            source_reference,
            derived_reference,
            source_digest: source::SOURCE.into(),
            derived_digest,
            cpus,
            service_cpus,
            ctr: Vec::new(),
            docker: Vec::new(),
            baseline: Value::Null,
            provenance: Value::Null,
            manifest: manifest.clone(),
            frozen,
            path: lab.clone(),
            prefix,
            evidence: evidence.to_owned(),
            index: 0,
            roles: BTreeMap::new(),
            _lock: lock,
            artifacts: BTreeMap::new(),
        };
        let inspected: Value = serde_json::from_str(&this.command(&strings(&[
            "docker",
            "inspect",
            &this.container.clone(),
        ]))?)?;
        let inspected = inspected
            .as_array()
            .and_then(|a| a.first())
            .context("missing Docker inspect")?;
        ensure!(
            inspected["Config"]["Labels"]["io.pvisor.overlaybd-lab"] == id
                && inspected["Image"] == manifest["image_id"]
                && inspected["HostConfig"]["CgroupnsMode"] == "host"
                && matches!(
                    inspected["HostConfig"]["PidMode"].as_str(),
                    Some("" | "private") | None
                )
                && inspected["State"]["Running"] == true,
            "Docker identity/namespace/running mismatch"
        );
        ensure!(
            inspected["Mounts"]
                .as_array()
                .context("missing mounts")?
                .iter()
                .any(|m| {
                    m["Destination"] == "/lab"
                        && m["RW"] == true
                        && m["Source"]
                            .as_str()
                            .and_then(|s| Path::new(s).canonicalize().ok())
                            .as_ref()
                            == Some(&lab)
                }),
            "lab bind ownership mismatch"
        );
        this.container = inspected["Id"].as_str().context("container ID")?.into();
        this.docker = strings(&["docker", "exec", &this.container]);
        this.ctr = this.docker.clone();
        this.ctr.extend(strings(&[
            CTR,
            "--address",
            SOCKET,
            "--namespace",
            namespace,
        ]));
        for name in ["source", "derived"] {
            let digest = if name == "source" {
                source::SOURCE
            } else {
                &this.derived_digest
            }
            .to_owned();
            this.registry_manifest(port, suffix, &digest, name, evidence)?;
            let image = load(&evidence.join(format!("{name}-manifest.json")))?;
            this.roles.insert(
                image["config"]["digest"]
                    .as_str()
                    .context("config digest")?
                    .into(),
                "metadata".into(),
            );
            for layer in image["layers"].as_array().context("manifest layers")? {
                let target=layer["annotations"]["containerd.io/snapshot/overlaybd/turbo-oci/target-digest"].as_str();
                this.roles.insert(
                    layer["digest"].as_str().context("layer digest")?.into(),
                    if target.is_some() {
                        "metadata"
                    } else {
                        "content"
                    }
                    .into(),
                );
                if let Some(target) = target {
                    source::hex(target)?;
                    this.roles.insert(target.into(), "content".into());
                }
            }
        }
        for name in [
            "lab.json",
            "source-image.json",
            "containerd.toml",
            "snapshotter.json",
            "overlaybd.json",
        ] {
            source::freeze(&lab.join(name), &evidence.join(name))?;
            this.artifacts.insert(
                lab.join(name).display().to_string(),
                source::digest(&lab.join(name))?,
            );
        }
        source::freeze(
            &smoke.join("frozen-images.json"),
            &evidence.join("frozen-images.json"),
        )?;
        this.artifacts.insert(
            smoke.join("frozen-images.json").display().to_string(),
            source::digest(&smoke.join("frozen-images.json"))?,
        );
        for name in [
            "source-manifest.json",
            "derived-manifest.json",
            "frozen-images.json",
        ] {
            this.artifacts.insert(
                evidence.join(name).display().to_string(),
                source::digest(&evidence.join(name))?,
            );
        }
        let mut argv = this.docker.clone();
        argv.extend(strings(&["sha256sum"]));
        argv.extend(BINARIES.map(str::to_owned));
        let raw = this.command(&argv)?;
        let mut binaries = BTreeMap::new();
        for line in raw.lines() {
            let (hash, path) = line
                .split_once(char::is_whitespace)
                .context("invalid tool hash")?;
            source::hex(&format!("sha256:{hash}"))?;
            binaries.insert(path.trim().to_owned(), hash.to_owned());
        }
        ensure!(
            binaries.len() == BINARIES.len() && BINARIES.iter().all(|p| binaries.contains_key(*p)),
            "missing lab tool identity"
        );
        this.provenance = json!({"lab_id":id,"docker_container_id":this.container,"lab_image_id":manifest["image_id"],
            "namespace":namespace,"source_digest":source::SOURCE,"derived_digest":this.derived_digest,
            "source_reference":this.source_reference,"derived_reference":this.derived_reference,
            "source_manifest_sha256":source::digest(&evidence.join("source-manifest.json"))?,
            "derived_manifest_sha256":source::digest(&evidence.join("derived-manifest.json"))?,
            "binaries":binaries,"configs":this.artifacts});
        save(&evidence.join("provenance.json"), &this.provenance)?;
        this.baseline = inventory()?;
        ensure!(
            this.baseline["targets"].as_object().unwrap().is_empty(),
            "live TCMU targets before experiment"
        );
        save(&evidence.join("host-before.json"), &this.baseline)?;
        this.no_tasks(&[])?;
        this.pin_services()?;
        Ok(this)
    }
    pub(super) fn command(&mut self, argv: &[String]) -> Result<String> {
        self.index += 1;
        let folder = self.evidence.join(format!("command-{:06}", self.index));
        String::from_utf8(process::command(argv, &folder, 120)?).context("non-UTF8 command output")
    }
    pub(super) fn docker_command(&mut self, args: &[String]) -> Result<String> {
        let mut argv = self.docker.clone();
        argv.extend_from_slice(args);
        self.command(&argv)
    }
    pub(super) fn ctr_command(&mut self, args: &[String]) -> Result<String> {
        let mut argv = self.ctr.clone();
        argv.extend_from_slice(args);
        self.command(&argv)
    }
    fn registry_manifest(
        &mut self,
        port: u64,
        repository: &str,
        digest: &str,
        name: &str,
        out: &Path,
    ) -> Result<()> {
        let body = out.join(format!("{name}-manifest.json"));
        let header = out.join(format!("{name}-headers.txt"));
        self.command(&strings(&["curl","--fail","--silent","--show-error","--max-time","30","--noproxy","*",
            "-H","Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
            "-D",header.to_str().context("header path")?,"-o",body.to_str().context("body path")?,
            &format!("http://127.0.0.1:{port}/v2/{repository}/manifests/{digest}")]))?;
        let headers = fs::read_to_string(header)?;
        ensure!(
            headers
                .lines()
                .any(|l| l
                    .split_once(':')
                    .is_some_and(|(k, v)| k.eq_ignore_ascii_case("Docker-Content-Digest")
                        && v.trim() == digest)),
            "registry declared digest mismatch"
        );
        ensure!(
            fs::metadata(&body)?.len() <= 8 * 1024 * 1024
                && source::digest(&body)? == source::hex(digest)?,
            "registry body digest/size mismatch"
        );
        let parsed = load(&body)?;
        ensure!(
            parsed.get("config").is_some()
                && parsed["layers"].is_array()
                && parsed.get("manifests").is_none(),
            "single platform registry manifest required"
        );
        Ok(())
    }
    pub(super) fn no_tasks(&mut self, identities: &[&str]) -> Result<()> {
        for resource in ["tasks", "containers"] {
            let raw = self.ctr_command(&strings(&[resource, "list"]))?;
            let rows: Vec<_> = raw
                .lines()
                .skip(1)
                .filter_map(|l| l.split_whitespace().next())
                .collect();
            ensure!(
                (!identities.is_empty() || rows.is_empty())
                    && !identities.iter().any(|id| rows.contains(id)),
                "private namespace retains {resource}: {rows:?}"
            );
        }
        Ok(())
    }
    pub(super) fn audit_inventory(&mut self, evidence: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let after = inventory()?;
            let clean = ["targets", "block_devices", "uio", "boot_id"]
                .iter()
                .all(|key| self.baseline[key] == after[key]);
            if clean || Instant::now() >= deadline {
                save(&evidence.join("host-after.json"), &after)?;
                save(
                    &evidence.join("host-delta.json"),
                    &json!({"unchanged":clean,"baseline":self.baseline,"after":after}),
                )?;
                ensure!(
                    clean,
                    "host TCMU/block/UIO inventory changed; retain lab for manual recovery"
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    pub(super) fn pin_services(&mut self) -> Result<()> {
        let script = r#"set -eu
cpus=$1
for process in /proc/[0-9]*; do
 exe=$(readlink "$process/exe" 2>/dev/null) || continue
 case "$exe" in /bin/registry) service=registry;; /opt/overlaybd/bin/overlaybd-tcmu) service=tcmu;; /opt/overlaybd/snapshotter/overlaybd-snapshotter) service=snapshotter;; /usr/local/bin/containerd) service=containerd;; *) continue;; esac
 pid=${process##*/}
 printf 'SERVICE %s %s\n' "$service" "$pid"
 /lab/tools/taskset -apc "$cpus" "$pid"
 for thread in "$process"/task/[0-9]*; do
  mask=$(awk '/^Cpus_allowed_list:/ {print $2}' "$thread/status")
  printf 'AFFINITY %s %s %s %s\n' "$service" "$pid" "${thread##*/}" "$mask"
 done
done"#;
        let raw = self.docker_command(&strings(&[
            "sh",
            "-ec",
            script,
            "sh",
            &self.service_cpus.clone(),
        ]))?;
        let mut services = BTreeMap::new();
        let mut seen = BTreeMap::new();
        let expected = process::cpus(&self.service_cpus)?;
        for line in raw.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.first() == Some(&"SERVICE") {
                ensure!(
                    parts.len() == 3 && services.insert(parts[1], parts[2]).is_none(),
                    "duplicate service"
                );
            }
            if parts.first() == Some(&"AFFINITY") {
                ensure!(
                    parts.len() == 5 && process::cpus(parts[4])? == expected,
                    "service thread affinity mismatch"
                );
                seen.insert(parts[1], true);
            }
        }
        ensure!(
            services.len() == 4
                && ["registry", "tcmu", "snapshotter", "containerd"]
                    .iter()
                    .all(|s| services.contains_key(s) && seen.contains_key(s)),
            "missing service/affinity"
        );
        save(
            &self
                .evidence
                .join(format!("service-affinity-{}.json", self.index)),
            &json!({"raw":raw,"cpus":expected}),
        )?;
        Ok(())
    }
    pub(super) fn registry_cursor(&self) -> Result<Cursor> {
        let m = fs::metadata(self.path.join("log/registry.log"))?;
        Ok(Cursor {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
        })
    }
    pub(super) fn registry_observation(&self, cursor: Cursor, evidence: &Path) -> Result<Traffic> {
        let path = self.path.join("log/registry.log");
        let m = fs::metadata(&path)?;
        ensure!(
            m.dev() == cursor.dev && m.ino() == cursor.ino && m.len() >= cursor.size,
            "registry log changed identity/truncated"
        );
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(cursor.size))?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;
        fs::write(evidence.join("registry-responses.log"), &raw)?;
        let (mut rows, mut total) = (Vec::new(), Traffic::default());
        let mut blob_bytes = 0u64;
        for line in std::str::from_utf8(&raw)?
            .lines()
            .filter(|l| l.contains("msg=\"response completed\""))
        {
            let field = |key: &str| -> Result<String> {
                let offset = line
                    .find(&format!("{key}="))
                    .context("registry field missing")?
                    + key.len()
                    + 1;
                let rest = &line[offset..];
                if rest.starts_with('"') {
                    let mut de = serde_json::Deserializer::from_str(rest);
                    Ok(String::deserialize(&mut de)?)
                } else {
                    Ok(rest
                        .split_whitespace()
                        .next()
                        .context("empty registry field")?
                        .into())
                }
            };
            let method = field("http.request.method")?;
            let uri = field("http.request.uri")?;
            let status: u16 = field("http.response.status")?.parse()?;
            let bytes: u64 = field("http.response.written")?.parse()?;
            ensure!(
                [200, 206].contains(&status)
                    && ["GET", "HEAD"].contains(&method.as_str())
                    && (method != "HEAD" || bytes == 0),
                "registry error/unsupported response"
            );
            let role = if uri.contains("/blobs/") {
                self.roles
                    .get(uri.split('?').next().unwrap().rsplit('/').next().unwrap())
                    .map(String::as_str)
                    .unwrap_or("unclassified")
            } else {
                "metadata"
            };
            ensure!(
                role != "unclassified" || bytes == 0,
                "unclassified registry blob payload"
            );
            if role == "content" {
                total.content_bytes += bytes;
            } else if role == "metadata" {
                total.metadata_bytes += bytes;
            }
            total.response_bytes += bytes;
            if uri.contains("/blobs/") && method == "GET" {
                blob_bytes += bytes;
            }
            rows.push(
                json!({"method":method,"uri":uri,"status":status,"body_bytes":bytes,"role":role}),
            );
        }
        save(
            &evidence.join("registry-responses.json"),
            &json!({"requests":rows,"totals":total,"blob_body_bytes":blob_bytes,
            "scope":"completion-observation","ready_boundary_available":false,"accounting":"response body only, not HTTP/TCP wire bytes"}),
        )?;
        Ok(total)
    }
    pub(super) fn cleanup(&mut self, id: &str, evidence: &Path) -> Result<()> {
        self.no_tasks(&[id])?;
        let prefix = self.prefix.clone();
        self.docker_command(&strings(&["sh","-ec",
            "for d in \"$1\" \"$2\"; do test ! -d \"$d\" || { test -z \"$(cat \"$d/cgroup.procs\")\"; rmdir -- \"$d\"; }; done",
            "sh",&format!("/sys/fs/cgroup{prefix}/{id}"),&format!("/sys/fs/cgroup{prefix}")]))?;
        self.audit_inventory(evidence)
    }
    pub(super) fn check_artifacts(&mut self) -> Result<()> {
        for (path, expected) in &self.artifacts {
            ensure!(
                source::digest(Path::new(path))? == *expected,
                "lab config changed"
            );
        }
        ensure!(
            load(&self.evidence.join("frozen-images.json"))? == self.frozen,
            "frozen image identity changed"
        );
        let mut argv = self.docker.clone();
        argv.push("sha256sum".into());
        argv.extend(BINARIES.map(str::to_owned));
        let raw = self.command(&argv)?;
        for line in raw.lines() {
            let (hash, path) = line
                .split_once(char::is_whitespace)
                .context("invalid hash")?;
            ensure!(
                self.provenance["binaries"][path.trim()] == hash,
                "lab tool changed"
            );
        }
        Ok(())
    }
    pub(super) fn reset_client(&mut self, evidence: &Path) -> Result<()> {
        let overlay = load(&self.path.join("overlaybd.json"))?;
        let snapshotter = load(&self.path.join("snapshotter.json"))?;
        ensure!(
            overlay["cacheConfig"]["cacheType"] == "file"
                && overlay["cacheConfig"]["cacheDir"] == "/lab/cache/registry"
                && overlay["gzipCacheConfig"]["cacheDir"] == "/lab/cache/gzip"
                && overlay["download"]["enable"] == false
                && overlay["p2pConfig"]["enable"].as_bool().unwrap_or(false) == false
                && snapshotter["root"]
                    == "/lab/containerd-root/io.containerd.snapshotter.v1.overlaybd"
                && snapshotter["address"] == "/lab/run/overlaybd.sock"
                && snapshotter["autoRemoveDev"] == true,
            "unsupported private reset layout"
        );
        let toml = fs::read_to_string(self.path.join("containerd.toml"))?;
        for (key, value) in [
            ("root", "/lab/containerd-root"),
            ("state", "/lab/run/containerd"),
        ] {
            let values: Vec<_> = toml
                .lines()
                .filter_map(|l| l.split_once('='))
                .filter(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().trim_matches('"'))
                .collect();
            ensure!(values == [value], "containerd mutable paths differ");
        }
        let taskset = source::digest(&self.path.join("tools/taskset"))?;
        let archive = format!("retired-client-state-{}", process::nonce()?);
        let worker = format!(
            "pvisor-overlaybd-reset-{}-{}",
            &self.manifest["id"].as_str().unwrap(),
            &process::nonce()?[..8]
        );
        let mut receipt = json!({"valid":false,"phase":"preflight","retired_state":self.path.join(&archive),
            "reset_paths":["containerd-root","cache","run"],"preserved":["registry","conversion","host page cache"],
            "worker_container":worker,"taskset_sha256":taskset});
        save(&evidence.join("reset-receipt.json"), &receipt)?;
        fs::write(
            evidence.join("recovery.txt"),
            format!(
                "Owned reset only. If incomplete, leave lab stopped. Inspect retained archive {}, Docker container {}, worker {}. Never merge retired state into fresh client directories or delete global devices.\n",
                archive, self.container, worker
            ),
        )?;
        let namespaces = self.ctr_command(&strings(&["namespaces", "list"]))?;
        for ns in namespaces
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next())
        {
            for resource in ["tasks", "containers", "leases"] {
                let mut argv = self.docker.clone();
                argv.extend(strings(&[
                    CTR,
                    "--address",
                    SOCKET,
                    "--namespace",
                    ns,
                    resource,
                    "list",
                ]));
                ensure!(
                    self.command(&argv)?
                        .lines()
                        .skip(1)
                        .all(|l| l.trim().is_empty()),
                    "cold reset refuses busy namespace"
                );
            }
        }
        let prefix = self.prefix.clone();
        self.docker_command(&strings(&["sh","-ec",r#"set -eu
root=/sys/fs/cgroup$1
if test -d "$root"; then
 test -z "$(cat "$root/cgroup.procs")"
 test -z "$(find "$root" -mindepth 1 -maxdepth 1 -type d -print -quit)"
 rmdir -- "$root"
fi
awk '$5 == "/lab/containerd-root" || index($5,"/lab/containerd-root/")==1 {print; bad=1} END {exit bad}' /proc/self/mountinfo
for process in /proc/[0-9]*; do
 exe=$(readlink "$process/exe" 2>/dev/null) || continue
 case "$exe" in */ctr|*/convertor|*/runc|*/containerd-shim-runc-v2) printf 'busy: %s %s\n' "$process" "$exe"; exit 1;; esac
done"#,"sh",&prefix]))?;
        let before_stop = evidence.join("reset-before-stop");
        fs::create_dir(&before_stop)?;
        self.audit_inventory(&before_stop)?;
        receipt["phase"] = "stopping-owned-lab".into();
        save(&evidence.join("reset-receipt.json"), &receipt)?;
        self.command(&strings(&[
            "docker",
            "stop",
            "--time",
            "30",
            &self.container.clone(),
        ]))?;
        let inspected: Value = serde_json::from_str(&self.command(&strings(&[
            "docker",
            "inspect",
            &self.container.clone(),
        ]))?)?;
        let state = &inspected[0]["State"];
        ensure!(
            state["Running"] == false
                && state["OOMKilled"] == false
                && state["ExitCode"] == 0
                && state["Error"].as_str().unwrap_or("").is_empty(),
            "lab did not stop normally; preserve stopped lab"
        );
        let after_stop = evidence.join("reset-after-stop");
        fs::create_dir(&after_stop)?;
        self.audit_inventory(&after_stop)?;
        ensure!(
            !self
                .command(&strings(&["ps", "-eo", "comm="]))?
                .lines()
                .any(|l| l.trim() == "overlaybd-tcmu"),
            "another shared TCMU handler visible"
        );
        receipt["phase"] = "archiving-private-state".into();
        save(&evidence.join("reset-receipt.json"), &receipt)?;
        let script = r#"set -eu
archive=/lab/$1
test ! -e "$archive"
for name in containerd-root cache run registry conversion log; do test ! -L "/lab/$name"; done
mkdir "$archive"; mkdir "$archive/service-logs"
for log in /lab/log/*; do test -f "$log" && test ! -L "$log" || continue; cp -p -- "$log" "$archive/service-logs/"; done
stat -c 'PRESERVED %n %d:%i' /lab/registry /lab/conversion
for name in containerd-root cache run; do
 if test -e "/lab/$name"; then test -d "/lab/$name"; mv -- "/lab/$name" "$archive/$name"; fi
 mkdir "/lab/$name"
 test -z "$(find "/lab/$name" -mindepth 1 -maxdepth 1 -print -quit)"
 printf 'EMPTY %s\n' "$name"
done
mkdir /lab/cache/registry /lab/cache/gzip
stat -c 'PRESERVED %n %d:%i' /lab/registry /lab/conversion"#;
        let mount = format!("type=bind,src={},dst=/lab", self.path.display());
        let raw = self.command(&strings(&[
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            &worker,
            "--label",
            &format!(
                "io.pvisor.overlaybd-lab={}",
                self.manifest["id"].as_str().unwrap()
            ),
            "--label",
            "io.pvisor.overlaybd-role=client-reset",
            "--network",
            "none",
            "--security-opt",
            "label=disable",
            "--cpuset-cpus",
            &self.service_cpus.clone(),
            "--cpus",
            "2",
            "--mount",
            &mount,
            "--entrypoint",
            "/bin/sh",
            &self.manifest["image_id"].as_str().unwrap().to_owned(),
            "-ec",
            script,
            "reset-private-client",
            &archive,
        ]))?;
        let preserved: Vec<_> = raw
            .lines()
            .filter(|l| l.starts_with("PRESERVED "))
            .collect();
        let mut empty: Vec<_> = raw.lines().filter(|l| l.starts_with("EMPTY ")).collect();
        empty.sort_unstable();
        ensure!(
            empty == ["EMPTY cache", "EMPTY containerd-root", "EMPTY run"]
                && preserved.len() == 4
                && preserved[..2] == preserved[2..],
            "private state reset/preserved directory proof failed"
        );
        receipt["phase"] = "starting-owned-lab".into();
        receipt["preserved_directory_identity"] = json!(&preserved[..2]);
        save(&evidence.join("reset-receipt.json"), &receipt)?;
        self.command(&strings(&["docker", "start", &self.container.clone()]))?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self
                .ctr_command(&strings(&["plugins", "list"]))
                .is_ok_and(|s| {
                    s.lines().any(|l| {
                        l.contains("snapshotter")
                            && l.contains("overlaybd")
                            && l.split_whitespace().last() == Some("ok")
                    })
                })
            {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "owned lab restart did not become healthy; retain reset journal"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        self.pin_services()?;
        let repository = self
            .source_reference
            .split('@')
            .next()
            .unwrap()
            .split_once('/')
            .unwrap()
            .1
            .to_owned();
        let port = self.manifest["port"].as_u64().unwrap();
        for (name, digest) in [
            ("source", self.source_digest.clone()),
            ("derived", self.derived_digest.clone()),
        ] {
            self.registry_manifest(port, &repository, &digest, name, evidence)?;
        }
        let namespaces = self.ctr_command(&strings(&["namespaces", "list"]))?;
        let ns = self.manifest["namespace"].as_str().unwrap().to_owned();
        if !namespaces
            .lines()
            .skip(1)
            .any(|l| l.split_whitespace().next() == Some(ns.as_str()))
        {
            self.ctr_command(&strings(&["namespaces", "create", &ns]))?;
        }
        for resource in ["images", "content", "tasks", "containers", "leases"] {
            ensure!(
                self.ctr_command(&strings(&[resource, "list"]))?
                    .lines()
                    .skip(1)
                    .all(|l| l.trim().is_empty()),
                "reset retained client resources"
            );
        }
        for snapshotter in ["overlayfs", "overlaybd"] {
            ensure!(
                self.ctr_command(&strings(&[
                    "snapshots",
                    "--snapshotter",
                    snapshotter,
                    "list"
                ]))?
                .lines()
                .skip(1)
                .all(|l| l.trim().is_empty()),
                "reset retained snapshots"
            );
        }
        let after_start = evidence.join("reset-after-start");
        fs::create_dir(&after_start)?;
        self.audit_inventory(&after_start)?;
        let raw = self.docker_command(&strings(&["sha256sum", "/lab/tools/taskset"]))?;
        ensure!(
            raw.split_whitespace().next() == Some(taskset.as_str()),
            "taskset changed across reset"
        );
        receipt["valid"] = true.into();
        receipt["phase"] = "empty-client-ready".into();
        receipt["metadata_and_content_empty"] = true.into();
        receipt["registry_process_restarted"] = true.into();
        save(&evidence.join("reset-receipt.json"), &receipt)?;
        Ok(())
    }
}
use serde::Deserialize;
pub(super) fn inventory() -> Result<Value> {
    let mut result = json!({"boot_id":fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim(),
        "mountinfo":fs::read_to_string("/proc/self/mountinfo")?,"targets":{},"block_devices":{},"uio":{},"errors":[]});
    let root = Path::new("/sys/kernel/config/target/core");
    ensure!(
        root.is_dir(),
        "configfs target/core unavailable; cannot claim cleanup inventory"
    );
    for group in fs::read_dir(root)? {
        let group = group?.path();
        if !group.is_dir()
            || !group
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("user_")
        {
            continue;
        }
        for path in fs::read_dir(group)? {
            let path = path?.path();
            if !path.is_dir() {
                continue;
            }
            let mut row = json!({"path":path,"inode":fs::metadata(&path)?.ino()});
            for attr in ["info", "udev_path", "enable"] {
                if path.join(attr).exists() {
                    row[attr] = fs::read_to_string(path.join(attr))?.trim().into();
                }
            }
            result["targets"][path.to_str().context("nonUTF8 sysfs")?] = row;
        }
    }
    for (kind, root) in [
        ("block_devices", "/sys/class/block"),
        ("uio", "/sys/class/uio"),
    ] {
        let root = Path::new(root);
        if kind == "uio" && !root.exists() {
            continue;
        }
        for path in fs::read_dir(root)? {
            let path = path?.path();
            let mut row =
                json!({"sysfs_path":path.canonicalize()?,"inode":fs::metadata(&path)?.ino()});
            for attr in ["dev", "name", "device/vendor", "device/model"] {
                if path.join(attr).exists() {
                    row[attr] = fs::read_to_string(path.join(attr))?.trim().into();
                }
            }
            result[kind][path
                .file_name()
                .unwrap()
                .to_str()
                .context("nonUTF8 device")?] = row;
        }
    }
    Ok(result)
}

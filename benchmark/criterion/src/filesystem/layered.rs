//! Nine-runtime layered workloads. Each operation is one randomized cohort of
//! fresh environments; three resident rounds remain correlated observations.
use super::support::*;
use anyhow::{Context, Result, bail, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub(super) const MODES: [&str; 9] = [
    "native",
    "host",
    "staged",
    "vm",
    "vm-writeback",
    "docker",
    "fc-system",
    "qemu",
    "qemu-microvm",
];
const LABEL: &str = "org.pvisor.benchmark.user-filesystem-trial";
pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
pub(super) fn cp(source: &Path, target: &Path) -> Result<()> {
    checked(
        Command::new("cp")
            .args(["-a", "--reflink=auto", "--sparse=always", "--"])
            .arg(source)
            .arg(target),
    )?;
    Ok(())
}
fn rebase(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(s) if s == old || s.starts_with(&format!("{old}/")) => {
            *s = format!("{new}{}", &s[old.len()..]);
        }
        Value::Array(items) => {
            for item in items {
                rebase(item, old, new);
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                rebase(item, old, new);
            }
        }
        _ => {}
    }
}
fn validate_row(row: &Value, case: &str) -> Result<()> {
    ensure!(
        row["id"] == case && row["correctness"] == true,
        "case correctness"
    );
    let work = row["work_ns"].as_u64().context("work clock")?;
    let sync = row["final_sync_ns"].as_u64().context("sync clock")?;
    ensure!(
        row["completed_ns"].as_u64() == work.checked_add(sync),
        "clock partition"
    );
    let expected = match case {
        "M01" | "M02" | "M03" => json!({"metadata_ops":2048}),
        "M04" => json!({"read_ops":192,"bytes_read":12<<20}),
        "M05" => json!({"read_ops":1024,"bytes_read":4<<20}),
        "M06" => json!({"write_ops":4096,"bytes_written":4<<20}),
        "M07" => json!({"write_ops":4,"bytes_written":4<<20}),
        "M08" => json!({"write_ops":1024,"bytes_written":4<<20}),
        "M09" => json!({"write_ops":256,"bytes_written":16<<20}),
        "M10" => json!({"write_ops":256,"bytes_written":1<<20,"periodic_fsyncs":256}),
        "M11" => json!({"write_ops":1024,"bytes_written":4<<20,"read_ops":1024,"bytes_read":4<<20}),
        "M12" => {
            json!({"write_ops":128,"bytes_written":512<<10,"read_ops":128,"bytes_read":512<<10,"metadata_ops":384,"periodic_fsyncs":128})
        }
        "R01" | "R02" | "R03" | "R04" => {
            json!({"write_ops":262144,"bytes_written":1024<<20,"final_fsyncs":5})
        }
        "S01" | "S02" | "S03" => json!({}),
        _ => bail!("unknown case"),
    };
    for (key, value) in expected.as_object().unwrap() {
        ensure!(&row[key] == value, "wrong logical work/sync: {key}");
    }
    Ok(())
}
fn materialize(upper: &Path, view: &Path, case: &str) -> Result<Vec<String>> {
    fs::create_dir(view)?;
    fn visit(
        upper: &Path,
        source: &Path,
        target: &Path,
        case: &str,
        markers: &mut Vec<String>,
    ) -> Result<()> {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("output name")?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            ensure!(!meta.is_symlink(), "unexpected output symlink");
            if meta.is_dir() {
                fs::create_dir(target.join(name))?;
                visit(upper, &path, &target.join(name), case, markers)?;
            } else {
                ensure!(meta.is_file(), "unexpected special output file");
                if let Some(hidden) = name.strip_prefix(".wh.") {
                    let allowed = name == ".wh..wh..opq"
                        || (case == "M12"
                            && source == upper
                            && (0..128).any(|i| {
                                hidden == format!("temp-{i:04}.bin")
                                    || hidden == format!("renamed-{i:04}.bin")
                            }));
                    ensure!(allowed && meta.len() == 0, "unknown overlay marker");
                    ensure!(
                        name == ".wh..wh..opq" || !source.join(hidden).exists(),
                        "whiteout conflicts with output"
                    );
                    markers.push(path.strip_prefix(upper)?.to_string_lossy().into_owned());
                } else {
                    fs::hard_link(path, target.join(name))?;
                }
            }
        }
        Ok(())
    }
    let mut markers = vec![];
    if upper.exists() {
        visit(upper, upper, view, case, &mut markers)?;
    }
    Ok(markers)
}
pub(super) fn docker(config: &Value) -> Result<Command> {
    let mut command = Command::new(
        config["docker_binary"]
            .as_str()
            .unwrap_or("/usr/bin/docker"),
    );
    command.arg("--host").arg(string(config, "docker_host")?);
    clean(&mut command);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("DOCKER_") {
            command.env_remove(key);
        }
    }
    Ok(command)
}
struct Container {
    config: Value,
    name: String,
    token: String,
    folder: PathBuf,
    removed: std::cell::Cell<bool>,
}
impl Container {
    fn inspect(&self) -> Result<Value> {
        let text = checked(docker(&self.config)?.arg("inspect").arg(&self.name))?;
        let rows: Value = serde_json::from_str(&text)?;
        ensure!(
            rows.as_array().map(Vec::len) == Some(1)
                && rows[0]["Config"]["Labels"][LABEL] == self.token,
            "container ownership mismatch"
        );
        Ok(rows[0].clone())
    }
    fn cleanup(&self) -> Result<()> {
        if self.removed.get() {
            return Ok(());
        }
        let inspection = self.inspect()?;
        save(self.folder.join("docker-final-inspect.json"), &inspection)?;
        if !self.folder.join("docker-output").exists() {
            if inspection["State"]["Running"] == true {
                let _ = run(
                    docker(&self.config)?.arg("kill").arg(&self.name),
                    &self.folder.join("docker-stop"),
                    Duration::from_secs(10),
                    &[],
                );
            }
            let output = self.folder.join("docker-failed-output");
            fs::create_dir_all(&output)?;
            let _ = run(
                docker(&self.config)?
                    .arg("cp")
                    .arg(format!("{}:/bench-trial/work/output/.", self.name))
                    .arg(output),
                &self.folder.join("docker-failure-copy"),
                Duration::from_secs(30),
                &[],
            );
        }
        run(
            docker(&self.config)?
                .args(["rm", "--force"])
                .arg(&self.name),
            &self.folder.join("docker-cleanup"),
            Duration::from_secs(60),
            &[],
        )?;
        self.removed.set(true);
        Ok(())
    }
}
impl Drop for Container {
    fn drop(&mut self) {
        // A panic must not strand a running detached Docker task. Ownership is
        // checked again before this best-effort cleanup can affect anything.
        if !self.removed.get() {
            let _ = self.cleanup();
        }
    }
}
fn marker_command(worker: &Path, argv: &[String]) -> Command {
    let mut command = Command::new(worker);
    command.arg("launch").args(argv);
    command
}
struct Experiment {
    output: PathBuf,
    protocol: Value,
    toolroot: PathBuf,
    worker: PathBuf,
    cases: Vec<String>,
    index: u64,
    rng: Rng,
    failed: BTreeSet<(String, String)>,
    records: Vec<Value>,
    skipped: Vec<Value>,
}
impl Experiment {
    fn prepare(output: PathBuf) -> Result<Self> {
        // A prepared input is an artifact bundle, never an old measurement
        // cohort. Only frozen tools, payload, fixtures and reference templates
        // are copied; records, results, trials and history are not copied.
        fs::create_dir(&output)?;
        let fresh = std::env::var_os("PVISOR_BENCH_LAYERED_CONFIG").is_some();
        if fresh {
            super::layered_prepare::prepare(&output)?;
        }
        let source = if fresh {
            output.clone()
        } else {
            required("PVISOR_BENCH_LAYERED_INPUT")?
        };
        let mut protocol = load(source.join("protocol.json"))?;
        ensure!(
            protocol["benchmark_id"] == "B-FS-TOOLS"
                && protocol["rounds"] == 3
                && protocol["ram_mib"] == 512
                && protocol["trial_timeout_seconds"] == 90,
            "registered layered protocol required"
        );
        verify_hashes(&source, &protocol["artifacts"])?;
        ensure!(
            sha(&source.join("fixture-manifest.json"))?
                == string(&protocol, "fixture_manifest_sha256")?,
            "fixture manifest changed"
        );
        verify_hashes(
            &source.join("fixture-layer"),
            &load(source.join("fixture-manifest.json"))?,
        )?;
        let modes: BTreeSet<_> = protocol["backends"]
            .as_array()
            .context("backends")?
            .iter()
            .map(|v| v["id"].as_str().unwrap_or(""))
            .collect();
        ensure!(
            modes == MODES.into_iter().collect(),
            "all nine modes required; no cross-batch completion"
        );
        if !fresh {
            for name in [
                "harness",
                "bin",
                "fixture-layer",
                "fixture-manifest.json",
                "toolroot",
                "toolroot-receipt.json",
                "cli-build-receipt.json",
                "firmware-source.json",
                "reference-assets",
                "docker-assets",
            ] {
                cp(&source.join(name), &output.join(name))?;
            }
        }
        let old = source.to_str().context("UTF-8 source")?;
        let new = output.to_str().context("UTF-8 output")?;
        rebase(&mut protocol, old, new);
        let toolroot = output.join("toolroot");
        protocol["toolroot"] = json!(toolroot);
        protocol["fixture"] = json!(output.join("fixture-layer/fs-v2-fixture"));
        // Input manifests retain source bytes, while path-bearing receipts get
        // new independent copies and new digests, never rewriting their source.
        let mut receipt = load(output.join("toolroot-receipt.json"))?;
        rebase(&mut receipt, old, new);
        save(output.join("toolroot-receipt.json"), &receipt)?;
        protocol["artifacts"]["toolroot-receipt.json"] =
            json!(sha(&output.join("toolroot-receipt.json"))?);
        for item in receipt["installed"]
            .as_array()
            .context("installed")?
            .iter()
            .chain(
                receipt["runtime_dependencies"]
                    .as_array()
                    .context("dependencies")?,
            )
        {
            ensure!(
                sha(&toolroot.join(string(item, "path")?))? == string(item, "sha256")?,
                "prepared artifact changed"
            );
        }
        verify_hashes(&toolroot, &protocol["prepared_tools"])?;
        verify_hashes(&toolroot, &load(output.join("fixture-manifest.json"))?)?;
        // Freeze the supervisor executable separately from the static guest payload.
        let worker = output.join("bin/filesystem-worker");
        fs::copy(required("PVISOR_BENCH_FS_WORKER")?, &worker)?;
        let cases = protocol["cases"]
            .as_array()
            .context("cases")?
            .iter()
            .map(|v| Ok(v.as_str().context("case ID")?.to_owned()))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            cases.len() == 19 && cases.iter().collect::<BTreeSet<_>>().len() == 19,
            "full 19-case layered catalog required"
        );
        protocol["measurement_harness"] = json!({"language":"Rust","criterion_operation":"one randomized all-case nine-mode cohort",
            "minimum_statistical_samples":10,"warmups":"Criterion calibration also gets full correctness and raw records, never reuses environments",
            "source":include_str!("layered.rs"),"supervisor_sha256":sha(&worker)?});
        save(output.join("protocol.json"), &protocol)?;
        let parent = Path::new(string(
            &protocol["reference_configuration"]["docker"],
            "budget_cgroup",
        )?);
        let state = budget(Some(parent), false)?;
        save(output.join("budget-before.json"), &state)?;
        Ok(Self {
            output,
            protocol,
            toolroot,
            worker,
            cases,
            index: 0,
            rng: Rng(20261018),
            failed: BTreeSet::new(),
            records: vec![],
            skipped: vec![],
        })
    }
    fn namespace_command(&self, folder: &Path, case: &str, backend: &str) -> Result<Command> {
        for name in [
            "namespace-root",
            "work/input",
            "work/output",
            "stage",
            "runs",
            "config",
        ] {
            fs::create_dir_all(folder.join(name))?;
        }
        let payload: Vec<String> = [
            "/bench-artifacts/payload",
            case,
            "output",
            "input",
            "3",
            "512",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let mut argv = vec![];
        if backend == "native" {
            argv = payload;
        } else {
            argv.extend(
                [
                    "/bench-artifacts/pvisor",
                    "spawn",
                    "--no-agent-defaults",
                    "--pass-env",
                    "PYTHONDONTWRITEBYTECODE",
                    "--pass-env",
                    "PYTHONPYCACHEPREFIX",
                    "--overlaynet",
                    "off",
                    "--stdio",
                    "inherit",
                    "--timeout",
                    "180s",
                ]
                .into_iter()
                .map(str::to_owned),
            );
            if backend == "staged" || backend.starts_with("vm") {
                argv.extend(["--stage".into(), "/bench-trial/stage".into()]);
            }
            if backend.starts_with("vm") {
                argv.extend(
                    ["--vm", "--rootfs", "/", "--cpu", "2", "--memory", "512MiB"]
                        .into_iter()
                        .map(str::to_owned),
                );
            }
            argv.push("--".into());
            argv.extend(payload);
        }
        let mut parent = serde_json::Map::new();
        for name in ["user", "mnt", "pid"] {
            parent.insert(
                name.into(),
                json!(fs::read_link(format!("/proc/self/ns/{name}"))?),
            );
        }
        let config = json!({"schema":1,"backend":backend,"case_id":case,"toolroot":self.toolroot,"folder":folder,
            "namespace_root":folder.join("namespace-root"),"argv":argv,"parent_namespaces":parent});
        save(folder.join("launch.json"), &config)?;
        let mut command = Command::new("unshare");
        command
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--pid",
                "--fork",
                "--kill-child=KILL",
                "--propagation",
                "private",
            ])
            .arg(&self.worker)
            .arg("namespace")
            .arg(folder.join("launch.json"));
        clean(&mut command);
        for (key, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/tmp"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONPYCACHEPREFIX", "/__pvisor_reference_no_pyc__"),
            ("PVISOR_RUN_HOME", "/bench-trial/runs"),
            ("XDG_CONFIG_HOME", "/bench-trial/config"),
            ("PVISOR_FS_PROFILE", "0"),
            ("PVISOR_STARTUP_TIMING", "0"),
            ("PVISOR_STARTUP_PROFILE", "0"),
        ] {
            command.env(key, value);
        }
        if backend.starts_with("vm") {
            command
                .env(
                    "PVISOR_VM_FS_WRITEBACK",
                    if backend == "vm-writeback" { "1" } else { "0" },
                )
                .env("PVISOR_VM_FS_HOST_WRITEBACK", "0");
        }
        Ok(command)
    }
    fn container(&self, folder: &Path, case: &str) -> Result<(Command, Container)> {
        let config = self.protocol["reference_configuration"]["docker"].clone();
        use std::io::Read;
        let mut entropy = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut entropy)?;
        let token: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!("pvisor-fs-user-{token}");
        let container = Container {
            config: config.clone(),
            name: name.clone(),
            token: token.clone(),
            folder: folder.to_owned(),
            removed: std::cell::Cell::new(false),
        };
        save(
            folder.join("docker-launch.json"),
            &json!({"name":name,"token":token,"configuration":config}),
        )?;
        let mut create = docker(&config)?;
        create
            .args([
                "create",
                "--pull",
                "never",
                "--name",
                &name,
                "--label",
                &format!("{LABEL}={token}"),
                "--network",
                "none",
                "--cgroupns",
                "host",
                "--cgroup-parent",
                string(&config, "budget_slice_unit")?,
                "--workdir",
                "/bench-trial/work",
                "--mount",
            ])
            .arg(format!(
                "type=bind,src={},dst=/bench-trial/work/input,readonly",
                self.toolroot.join("fs-v2-fixture").display()
            ))
            .args(["--entrypoint", "/bench-artifacts/docker-runtime"]);
        for (key, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/tmp"),
            ("LC_ALL", "C"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONPYCACHEPREFIX", "/__pvisor_reference_no_pyc__"),
        ] {
            create.args(["--env", &format!("{key}={value}")]);
        }
        create.arg(string(&config, "image_id")?).args([
            "/bench-artifacts/payload",
            case,
            "output",
            "input",
            "3",
            "512",
        ]);
        if let Err(error) = run(
            &mut create,
            &folder.join("docker-create"),
            Duration::from_secs(120),
            &[],
        ) {
            // Only remove an object proven to bear our ownership label.
            let _ = container.cleanup();
            return Err(error);
        }
        let mut start = docker(&config)?;
        start.args(["start", "--attach", &name]);
        let argv = std::iter::once(start.get_program())
            .chain(start.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut command = marker_command(&self.worker, &argv);
        clean(&mut command);
        Ok((command, container))
    }
    fn reference(&self, folder: &Path, case: &str, backend: &str) -> Result<Command> {
        let assets = &self.protocol["reference_configuration"][backend]["assets"];
        for key in ["root", "work"] {
            cp(
                Path::new(string(assets, &format!("{key}_disk"))?),
                &folder.join(format!("{key}.ext4")),
            )?;
        }
        let paths = &assets["kernel_paths"];
        let boot = format!(
            "console=ttyS0 reboot={} panic=1 root=/dev/vda rw init=/bench/init quiet{} fsbench.case={case}",
            if backend == "qemu-microvm" { "t" } else { "k" },
            if backend == "qemu-microvm" {
                " pci=off"
            } else {
                ""
            }
        );
        let argv = if backend == "fc-system" {
            File::create(folder.join("firecracker.log"))?;
            let config = json!({"boot-source":{"kernel_image_path":paths["kernel"],"initrd_path":paths["initrd"],"boot_args":boot},
                "machine-config":{"vcpu_count":2,"mem_size_mib":512},
                "drives":(["root","work"].map(|name|json!({"drive_id":name,"path_on_host":folder.join(format!("{name}.ext4")),
                    "is_root_device":name=="root","is_read_only":false,"cache_type":"Writeback"}))),
                "logger":{"log_path":folder.join("firecracker.log"),"level":"Warning","show_level":true,"show_log_origin":true}});
            save(folder.join("firecracker.json"), &config)?;
            vec![
                string(&assets["runtimes"]["firecracker"], "executable")?.into(),
                "--no-api".into(),
                "--config-file".into(),
                folder
                    .join("firecracker.json")
                    .to_string_lossy()
                    .into_owned(),
            ]
        } else {
            let micro = backend == "qemu-microvm";
            let mut argv =
                vec![string(&assets["runtimes"]["qemu-system-x86_64"], "executable")?.to_owned()];
            argv.extend(
                [
                    "-machine",
                    if micro {
                        "microvm,acpi=on,x-option-roms=off,pit=off,pic=off,rtc=on"
                    } else {
                        "q35"
                    },
                    "-accel",
                    "kvm",
                    "-cpu",
                    "host",
                    "-smp",
                    "2",
                    "-m",
                    "512",
                    "-nodefaults",
                    "-L",
                    string(assets, "qemu_data_dir")?,
                    "-bios",
                    string(
                        &assets["qemu_firmware"][if micro { "qboot.rom" } else { "bios-256k.bin" }],
                        "path",
                    )?,
                    "-display",
                    "none",
                    "-serial",
                    "stdio",
                    "-no-reboot",
                    "-kernel",
                    string(paths, "vmlinuz")?,
                    "-initrd",
                    string(paths, "initrd")?,
                    "-append",
                    &boot,
                ]
                .into_iter()
                .map(str::to_owned),
            );
            for name in ["root", "work"] {
                argv.extend([
                    "-drive".into(),
                    format!(
                        "file={},format=raw,if=none,id={name},cache=writeback",
                        folder.join(format!("{name}.ext4")).display()
                    ),
                    "-device".into(),
                    format!(
                        "{},drive={name}",
                        if micro {
                            "virtio-blk-device"
                        } else {
                            "virtio-blk-pci"
                        }
                    ),
                ]);
            }
            argv
        };
        save(
            folder.join("reference-launch.json"),
            &json!({"argv":argv,"case":case,"backend":backend}),
        )?;
        let mut command = marker_command(&self.worker, &argv);
        clean(&mut command);
        Ok(command)
    }
    fn finalize_container(&self, container: &Container, text: &str) -> Result<PathBuf> {
        let value = container.inspect()?;
        save(container.folder.join("docker-inspect.json"), &value)?;
        ensure!(
            value["State"]["Running"] == false
                && value["State"]["ExitCode"] == 0
                && value["State"]["OOMKilled"] == false,
            "Docker task exit"
        );
        ensure!(
            value["GraphDriver"]["Name"] == "overlay2"
                && value["HostConfig"]["CgroupParent"] == container.config["budget_slice_unit"],
            "Docker storage/budget"
        );
        for mount in value["Mounts"].as_array().context("container mounts")? {
            ensure!(
                mount["Destination"] != "/bench-trial/work"
                    && mount["Destination"] != "/bench-trial/work/output",
                "output bind bypassed overlay2"
            );
        }
        let runtime = unique(text, "USER_WORKLOADS_DOCKER_RUNTIME ")?;
        ensure!(
            runtime["affinity"] == json!([0, 1]) && runtime["root_filesystem"] == "overlay",
            "Docker runtime receipt"
        );
        let budget = string(&container.config, "budget_cgroup")?
            .strip_prefix("/sys/fs/cgroup/")
            .context("cgroup path")?;
        ensure!(
            string(&runtime, "cgroup_namespace_path")?.starts_with(&format!("0::/{budget}/")),
            "Docker payload outside parent budget"
        );
        let output = container.folder.join("docker-output");
        fs::create_dir(&output)?;
        run(
            docker(&container.config)?
                .arg("cp")
                .arg(format!("{}:/bench-trial/work/output/.", container.name))
                .arg(&output),
            &container.folder.join("docker-copy"),
            Duration::from_secs(300),
            &[],
        )?;
        Ok(output)
    }
    fn one(&self, case: &str, index: u64, backend: &str) -> Result<Value> {
        let folder = self
            .output
            .join("trials")
            .join(format!("{case}-{index:08}-{backend}"));
        fs::create_dir_all(&folder)?;
        let mut container = None;
        let result = (|| -> Result<Value> {
            let mut command = if backend == "docker" {
                let (command, owned) = self.container(&folder, case)?;
                container = Some(owned);
                command
            } else if ["fc-system", "qemu", "qemu-microvm"].contains(&backend) {
                self.reference(&folder, case, backend)?
            } else {
                self.namespace_command(&folder, case, backend)?
            };
            let parent = Path::new(string(
                &self.protocol["reference_configuration"]["docker"],
                "budget_cgroup",
            )?);
            let before = budget(Some(parent), false)?;
            // Stream observer timestamps Ready/DONE independently of worker clocks.
            let (wall, text, times) = run_marked(&mut command, &folder, Duration::from_secs(90))?;
            let after = budget(Some(parent), false)?;
            unchanged_events(&before, &after)?;
            let rows = records(&text, "WORKLOADS_V2_RESULT ")?;
            ensure!(rows.len() == 3, "exact three resident rounds required");
            let mut rounds = BTreeSet::new();
            for row in &rows {
                validate_row(row, case)?;
                ensure!(
                    rounds.insert(row["round"].as_u64().context("round")?),
                    "duplicate round"
                );
            }
            ensure!(rounds == [1, 2, 3].into_iter().collect(), "missing round");
            unique(&text, "WORKLOADS_V2_READY ")?;
            unique(&text, "WORKLOADS_V2_DONE ")?;
            ensure!(
                !fs::read_to_string(folder.join("stderr.log"))?.contains("pvisor-fs-profile "),
                "diagnostic timing contamination"
            );
            let mut overlay = false;
            let work = if let Some(owned) = &container {
                self.finalize_container(owned, &text)?
            } else if ["fc-system", "qemu", "qemu-microvm"].contains(&backend) {
                for failure in ["kernel panic", "not syncing", "bug:", "oops:"] {
                    ensure!(
                        !text.to_lowercase().contains(failure),
                        "reference guest failure"
                    );
                }
                ensure!(
                    unique(&text, "USER_WORKLOADS_VM_EXIT ")?
                        == json!({"returncode":0,"teardown_ok":true}),
                    "reference clean teardown"
                );
                run(
                    Command::new("e2fsck")
                        .args(["-f", "-n"])
                        .arg(folder.join("work.ext4")),
                    &folder.join("fsck"),
                    Duration::from_secs(120),
                    &[],
                )?;
                let view = folder.join("reference-output");
                fs::create_dir(&view)?;
                run(
                    Command::new("debugfs")
                        .arg("-R")
                        .arg(format!("rdump /output {}", view.display()))
                        .arg(folder.join("work.ext4")),
                    &folder.join("export"),
                    Duration::from_secs(300),
                    &[],
                )?;
                ensure!(view.join("output").is_dir(), "reference output missing");
                view.join("output")
            } else if backend == "staged" || backend.starts_with("vm") {
                ensure!(
                    fs::read_dir(folder.join("work/output"))?.next().is_none(),
                    "staged write escaped to source"
                );
                let names: BTreeSet<_> = fs::read_dir(folder.join("work"))?
                    .map(|e| e.map(|e| e.file_name()))
                    .collect::<std::io::Result<_>>()?;
                ensure!(
                    names == ["input".into(), "output".into()].into_iter().collect(),
                    "source workspace inventory"
                );
                overlay = true;
                folder.join("stage/upper/output")
            } else {
                folder.join("work/output")
            };
            let view = if overlay {
                let view = folder.join("host-view");
                let markers = materialize(&work, &view, case)?;
                save(folder.join("overlay-markers.json"), &json!(markers))?;
                view
            } else {
                work
            };
            let (_, verified) = run(
                Command::new(self.output.join("bin/payload"))
                    .args(["--verify", case])
                    .arg(&view)
                    .arg(string(&self.protocol, "fixture")?)
                    .args(["3", "512"]),
                &folder.join("host-oracle"),
                Duration::from_secs(180),
                &[],
            )?;
            ensure!(
                unique(&verified, "WORKLOADS_V2_VERIFIED ")?["verified"] == true,
                "independent host oracle"
            );
            Ok(
                json!({"valid":true,"case_id":case,"pair":index,"backend":backend,"rows":rows,"host_verified":true,
                "process_wall_ns":wall.as_nanos(),"external_clocks":times,"resource":{"before":before,"after":after}}),
            )
        })();
        let cleanup = if let Some(container) = container {
            container.cleanup()
        } else {
            Ok(())
        };
        let record = match (result, cleanup) {
            (Ok(record), Ok(())) => record,
            (result, cleanup) => {
                json!({"valid":false,"case_id":case,"pair":index,"backend":backend,
                "error":result.err().map(|e|format!("{e:#}")),"cleanup_error":cleanup.err().map(|e|format!("{e:#}"))})
            }
        };
        save(folder.join("result.json"), &record)?;
        if record["valid"] == true {
            for name in [
                "host-view",
                "work",
                "stage",
                "runs",
                "namespace-root",
                "reference-output",
                "docker-output",
            ] {
                let path = folder.join(name);
                if path.exists() {
                    ensure!(!fs::symlink_metadata(&path)?.is_symlink(), "cleanup alias");
                    fs::remove_dir_all(path)?;
                }
            }
            for name in ["root.ext4", "work.ext4"] {
                let path = folder.join(name);
                if path.exists() {
                    ensure!(!fs::symlink_metadata(&path)?.is_symlink(), "disk alias");
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(record)
    }
    fn cohort(&mut self, selected_case: &str, selected_backend: &str) -> Result<Duration> {
        let mut cases = self.cases.clone();
        self.rng.shuffle(&mut cases);
        let mut total = Duration::ZERO;
        for case in cases {
            let mut modes = MODES;
            self.rng.shuffle(&mut modes);
            save(
                self.output
                    .join(format!("order-{}-{case}.json", self.index)),
                &json!(modes),
            )?;
            for mode in modes {
                let cell = (case.clone(), mode.to_owned());
                if self.failed.contains(&cell) {
                    self.skipped.push(json!({"case":case,"backend":mode,"pair":self.index,"reason":"first cell failure; not executed"}));
                    save(self.output.join("skipped.json"), &json!(self.skipped))?;
                    continue;
                }
                let row = self.one(&case, self.index, mode)?;
                let valid = row["valid"] == true;
                if !valid {
                    self.failed.insert(cell);
                } else if case == selected_case && mode == selected_backend {
                    total += Duration::from_nanos(
                        row["rows"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|r| r["completed_ns"].as_u64().unwrap())
                            .sum(),
                    );
                }
                self.records.push(row);
                save(
                    self.output.join("records.partial.json"),
                    &json!(self.records),
                )?;
                if !valid {
                    save(
                        self.output.join("validation.json"),
                        &json!({"valid":false,"reason":"invalid matrix operation; no Criterion duration submitted",
                        "case":case,"backend":mode,"cohort":self.index}),
                    )?;
                    bail!(
                        "invalid {case}/{mode}; retained failure, matrix observation not submitted"
                    );
                }
            }
        }
        self.index += 1;
        Ok(total)
    }
}
/// Preserve per-line observation timestamps rather than estimating Ready from
/// the completion time or including namespace preparation in executor Ready.
fn monotonic_ns() -> Result<u128> {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } == 0,
        "clock"
    );
    Ok(t.tv_sec as u128 * 1_000_000_000 + t.tv_nsec as u128)
}
fn drain_lines_bounded<R: std::io::Read + std::os::fd::AsRawFd>(
    mut reader: R,
    deadline: std::time::Instant,
    mut chunk_observed: impl FnMut(&[u8]) -> Result<()>,
    mut line_observed: impl FnMut(&[u8], u128) -> Result<()>,
    mut tick_observed: impl FnMut() -> Result<()>,
) -> Result<()> {
    let fd = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0,
        "get stdout flags: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "set nonblocking stdout: {}",
        std::io::Error::last_os_error()
    );
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        ensure!(std::time::Instant::now() < deadline, "trial stdout timeout");
        tick_observed()?;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut descriptor, 1, 20) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if polled == 0 {
            continue;
        }
        ensure!(
            descriptor.revents & libc::POLLNVAL == 0,
            "invalid stdout descriptor"
        );
        loop {
            ensure!(std::time::Instant::now() < deadline, "trial stdout timeout");
            match reader.read(&mut buffer) {
                Ok(0) => {
                    if !pending.is_empty() {
                        line_observed(&pending, monotonic_ns()?)?;
                    }
                    return Ok(());
                }
                Ok(count) => {
                    let observed_at = monotonic_ns()?;
                    chunk_observed(&buffer[..count])?;
                    pending.extend_from_slice(&buffer[..count]);
                    while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        ensure!(std::time::Instant::now() < deadline, "trial stdout timeout");
                        let line: Vec<_> = pending.drain(..=end).collect();
                        line_observed(&line[..line.len() - 1], observed_at)?;
                    }
                    ensure!(
                        pending.len() <= 4 << 20,
                        "oversized unterminated stdout line"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}
fn run_marked(
    command: &mut Command,
    folder: &Path,
    timeout: Duration,
) -> Result<(Duration, String, Value)> {
    use std::{io::Write, process::Stdio, time::Instant};
    command
        .stdout(Stdio::piped())
        .stderr(File::create(folder.join("stderr.log"))?)
        .stdin(Stdio::null());
    save(
        folder.join("command.json"),
        &json!({"program":command.get_program().to_string_lossy(),"argv":command.get_args().map(|s|s.to_string_lossy().into_owned()).collect::<Vec<_>>()}),
    )?;
    let clock = monotonic_ns;
    let started = clock()?;
    let start = Instant::now();
    let mut child = spawn(command)?;
    let stdout = child.take_stdout().unwrap();
    let mut log = File::create(folder.join("stdout.log"))?;
    let mut text = String::new();
    let mut launch = None;
    let mut ready = None;
    let mut done = None;
    let mut observations = vec![];
    let outcome = (|| -> Result<()> {
        drain_lines_bounded(
            stdout,
            start + timeout,
            |bytes| {
                log.write_all(bytes)?;
                log.flush()?;
                Ok(())
            },
            |bytes, now| {
                let line = std::str::from_utf8(bytes)?;
                text.push_str(line);
                text.push('\n');
                if let Some(json) = line.strip_prefix("USER_WORKLOADS_LAUNCH ") {
                    ensure!(launch.is_none(), "duplicate launch");
                    launch = Some(
                        serde_json::from_str::<Value>(json)?["monotonic_ns"]
                            .as_u64()
                            .context("launch clock")? as u128,
                    );
                } else if line.starts_with("WORKLOADS_V2_READY ") {
                    ensure!(ready.replace(now).is_none(), "duplicate Ready");
                } else if line.starts_with("WORKLOADS_V2_DONE ") {
                    ensure!(done.replace(now).is_none(), "duplicate DONE");
                }
                Ok(())
            },
            || {
                observations.push(json!({"elapsed_ns":start.elapsed().as_nanos(),"status":fs::read_to_string(format!("/proc/{}/status",child.id())).ok()}));
                Ok(())
            },
        )?;
        loop {
            if let Some(status) = child.try_status()? {
                ensure!(status.success(), "executor exit {status}");
                break;
            }
            ensure!(start.elapsed() < timeout, "trial exit timeout");
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    })();
    drop(child);
    save(folder.join("observations.json"), &json!(observations))?;
    outcome?;
    let ended = clock()?;
    let (launch, ready, done) = (
        launch.context("launch missing")?,
        ready.context("Ready missing")?,
        done.context("DONE missing")?,
    );
    ensure!(
        started <= launch && launch <= ready && ready <= done && done <= ended,
        "external clock ordering"
    );
    Ok((
        start.elapsed(),
        text,
        json!({"started_ns":started,"launch_ns":launch,"ready_ns":ready-launch,
        "wrapper_prepare_ns":launch-started,"done_ns":done,"ended_ns":ended,"exit_after_done_ns":ended-done}),
    ))
}
pub fn register(c: &mut Criterion) {
    let output = output().expect("new layered evidence directory");
    let mut experiment =
        Experiment::prepare(output.join("experiment")).expect("layered asset preparation");
    let selected_case = std::env::var("PVISOR_BENCH_CASE").unwrap_or_else(|_| "S01".into());
    let selected_backend = std::env::var("PVISOR_BENCH_BACKEND").unwrap_or_else(|_| "vm".into());
    assert!(
        experiment.cases.contains(&selected_case) && MODES.contains(&selected_backend.as_str()),
        "unknown selected layered case/backend"
    );
    let mut group = c.benchmark_group("filesystem/user_workloads");
    // One actual iteration is a randomized full nineteen-case/nine-mode cohort.
    // Criterion's >=10 samples are a new protocol, never relabelled historical
    // N3. The raw independent unit remains each fresh case environment.
    group.sample_size(10).sampling_mode(SamplingMode::Flat);
    group.bench_function(
        format!("{selected_case}/{selected_backend}/three-rounds-ns"),
        |b| {
            b.iter_custom(|n| {
                let mut measured = Duration::ZERO;
                for _ in 0..n {
                    measured += experiment
                        .cohort(&selected_case, &selected_backend)
                        .expect("layered cohort");
                }
                measured
            })
        },
    );
    group.finish();
    verify_hashes(&experiment.output, &experiment.protocol["artifacts"])
        .expect("frozen artifacts unchanged");
    verify_hashes(&experiment.toolroot, &experiment.protocol["prepared_tools"])
        .expect("shared tools unchanged");
    verify_hashes(
        &experiment.toolroot,
        &load(experiment.output.join("fixture-manifest.json")).unwrap(),
    )
    .expect("shared fixtures unchanged");
    let parent = Path::new(
        string(
            &experiment.protocol["reference_configuration"]["docker"],
            "budget_cgroup",
        )
        .unwrap(),
    );
    let before = load(experiment.output.join("budget-before.json")).unwrap();
    let after = budget(Some(parent), false).unwrap();
    unchanged_events(&before, &after).unwrap();
    save(experiment.output.join("budget-after.json"), &after).unwrap();
    save(
        experiment.output.join("records.json"),
        &json!(experiment.records),
    )
    .unwrap();
    save(
        experiment.output.join("validation.json"),
        &json!({"valid":experiment.failed.is_empty(),
        "failed_cells":experiment.failed,"not_executed":experiment.skipped,
        "independent_cohorts":experiment.index,"resident_rounds":3,
        "selected_case":selected_case,"selected_backend":selected_backend,
        "protocol":"Criterion >=10 observations, separate from historical N3"}),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_sessions_spawn_workspace_relative_payload_without_library_override() {
        let temp = tempfile::tempdir().unwrap();
        let experiment = Experiment {
            output: temp.path().to_owned(),
            protocol: json!({}),
            toolroot: temp.path().join("toolroot"),
            worker: temp.path().join("worker"),
            cases: vec!["S02".into()],
            index: 0,
            rng: Rng(20261018),
            failed: BTreeSet::new(),
            records: vec![],
            skipped: vec![],
        };
        for backend in ["host", "staged", "vm", "vm-writeback"] {
            let folder = temp.path().join(backend);
            experiment
                .namespace_command(&folder, "S02", backend)
                .unwrap();
            let launch: Value =
                serde_json::from_slice(&fs::read(folder.join("launch.json")).unwrap()).unwrap();
            let argv: Vec<_> = launch["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            assert_eq!(&argv[..2], ["/bench-artifacts/pvisor", "spawn"]);
            assert_eq!(
                &argv[argv.len() - 6..],
                [
                    "/bench-artifacts/payload",
                    "S02",
                    "output",
                    "input",
                    "3",
                    "512"
                ]
            );
            if backend.starts_with("vm") {
                assert!(argv.contains(&"--vm"));
                assert!(!argv.contains(&"--vm-library-dir"));
            }
        }
    }
    #[test]
    fn independently_held_stdout_writer_does_not_prevent_bounded_teardown() {
        use std::io::Write;
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.write_all(b"unterminated stdout").unwrap();
        let start = std::time::Instant::now();
        let mut raw = Vec::new();
        let result = drain_lines_bounded(
            reader,
            start + Duration::from_millis(100),
            |bytes| {
                raw.extend_from_slice(bytes);
                Ok(())
            },
            |_, _| bail!("partial line must not be parsed"),
            || Ok(()),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(raw, b"unterminated stdout");
        // This test-owned writer is independent of every process group and
        // intentionally stays alive through the entire timeout and teardown.
        assert!(writer.write_all(b"after reader teardown").is_err());
    }
    #[test]
    fn nonblocking_drain_preserves_complete_and_final_unterminated_lines() {
        use std::io::Write;
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.write_all(b"first\nlast").unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        let mut lines = Vec::new();
        drain_lines_bounded(
            reader,
            std::time::Instant::now() + Duration::from_secs(1),
            |_| Ok(()),
            |line, _| {
                lines.push(line.to_vec());
                Ok(())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(lines, [b"first".to_vec(), b"last".to_vec()]);
    }
    #[test]
    fn rejects_wrong_partition() {
        assert!(validate_row(&json!({"id":"M01","correctness":true,"work_ns":10,"final_sync_ns":2,"completed_ns":11,"metadata_ops":2048}),"M01").is_err());
    }
    #[test]
    fn rejects_unknown_markers() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        fs::create_dir(&upper).unwrap();
        fs::write(upper.join(".wh.invented"), b"").unwrap();
        assert!(materialize(&upper, &temp.path().join("view"), "M06").is_err());
    }
    #[test]
    fn rejects_symlink_output() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        fs::create_dir(&upper).unwrap();
        std::os::unix::fs::symlink("/tmp", upper.join("escape")).unwrap();
        assert!(materialize(&upper, &temp.path().join("view"), "S03").is_err());
    }
    #[test]
    fn exited_leader_with_pipe_holding_descendant_cannot_hang_reader_join() {
        let temp = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let result = run_marked(
            Command::new("sh").args(["-c", "sleep 30 & echo $!; exit 0"]),
            temp.path(),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        let pid: u32 = fs::read_to_string(temp.path().join("stdout.log"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // An orphan can briefly remain a zombie until the host's init reaps it;
        // it must never remain an executing process retaining the pipe.
        for _ in 0..100 {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Ok(stat)
                    if stat.rsplit_once(") ").is_some_and(|(_, rest)| {
                        rest.starts_with("Z ") || rest.starts_with("X ")
                    }) =>
                {
                    return;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("pipe-holding descendant {pid} survived timeout teardown");
    }
}

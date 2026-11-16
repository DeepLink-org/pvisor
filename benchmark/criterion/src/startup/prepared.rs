use super::{
    evidence::{self, digest, read, save},
    observer::{self, Observation},
    subjects::{self, Backend, Inputs},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CANARY: &[u8] = b"unchanged startup input\n";

struct Affinity(libc::cpu_set_t);
impl Affinity {
    fn set(request: Option<String>) -> Result<(Self, String)> {
        let mut original: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&original), &mut original) }
                == 0,
            "read CPU affinity: {}",
            std::io::Error::last_os_error()
        );
        let cpus = if let Some(request) = request {
            request
                .split(',')
                .map(|part| part.parse::<usize>().context("CPU must be an integer"))
                .collect::<Result<Vec<_>>>()?
        } else {
            (0..libc::CPU_SETSIZE as usize)
                .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &original) })
                .take(2)
                .collect()
        };
        ensure!(
            cpus.len() == 2 && cpus[0] != cpus[1],
            "require two distinct available CPUs"
        );
        let mut selected: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for cpu in &cpus {
            ensure!(
                *cpu < libc::CPU_SETSIZE as usize && unsafe { libc::CPU_ISSET(*cpu, &original) },
                "requested CPU {cpu} unavailable"
            );
            unsafe {
                libc::CPU_SET(*cpu, &mut selected);
            }
        }
        ensure!(
            unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&selected), &selected) } == 0,
            "set CPU affinity: {}",
            std::io::Error::last_os_error()
        );
        Ok((
            Self(original),
            cpus.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ))
    }
}
impl Drop for Affinity {
    fn drop(&mut self) {
        unsafe {
            libc::sched_setaffinity(0, std::mem::size_of_val(&self.0), &self.0);
        }
    }
}

pub(super) struct Cohort {
    output: PathBuf,
    input: Inputs,
    artifacts: BTreeMap<PathBuf, String>,
    input_digest: Option<String>,
    sequence: u64,
    commands: u64,
    _affinity: Affinity,
}

fn resolve(program: &str) -> Result<PathBuf> {
    if program.contains('/') {
        return Ok(PathBuf::from(program).canonicalize()?);
    }
    for base in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let path = base.join(program);
        if path.is_file() {
            return Ok(path.canonicalize()?);
        }
    }
    bail!("required executable missing: {program}")
}

struct ChildGroup(std::process::Child);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

impl Cohort {
    pub(super) fn new(output: PathBuf, backends: &[Backend]) -> Result<Self> {
        ensure!(
            output.components().any(|part| part.as_os_str() == ".data"),
            "evidence must be retained under a .data directory"
        );
        // Create only the parent recursively. The final directory is exclusive,
        // preventing reuse of a historical or incomplete cohort.
        fs::create_dir_all(output.parent().context("output parent")?)?;
        let parent = output.parent().context("output parent")?.canonicalize()?;
        ensure!(
            parent.components().any(|part| part.as_os_str() == ".data"),
            "resolved evidence parent must remain under .data"
        );
        let output = parent.join(output.file_name().context("output leaf")?);
        fs::create_dir(&output).context("evidence directory must not already exist")?;
        let output = output.canonicalize()?;
        save(
            output.join("status.json"),
            &json!({"status":"preparing","engine":"rust-instant/criterion-0.7"}),
        )?;
        let result = Self::prepare(output.clone(), backends);
        if let Err(error) = &result {
            save(
                output.join("status.json"),
                &json!({"status":"failed","error":format!("{error:#}")}),
            )?;
        }
        result
    }

    fn prepare(output: PathBuf, backends: &[Backend]) -> Result<Self> {
        let (affinity, cpus) = Affinity::set(env::var("PVISOR_BENCH_CPUS").ok())?;
        let assets = if backends.iter().any(|backend| *backend != Backend::Native) {
            Some(
                PathBuf::from(
                    env::var_os("PVISOR_BENCHMARK_ASSETS")
                        .context("non-native startup requires PVISOR_BENCHMARK_ASSETS")?,
                )
                .canonicalize()?,
            )
        } else {
            None
        };
        let input_digest = assets
            .as_ref()
            .map(|path| evidence::verify_inputs(path))
            .transpose()?;
        let binary = if backends.iter().any(|backend| backend.pvisor()) {
            PathBuf::from(
                env::var_os("PVISOR_BENCH_BINARY")
                    .unwrap_or_else(|| "target/release/pvisor".into()),
            )
            .canonicalize()?
        } else {
            PathBuf::new()
        };
        let batch = format!(
            "pvisor-startup-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let mut cohort = Self {
            output,
            input: Inputs {
                binary,
                assets,
                kernel: None,
                vmlinuz: None,
                initrd: None,
                image: None,
                cpus,
                batch,
                programs: BTreeMap::new(),
            },
            input_digest,
            artifacts: BTreeMap::new(),
            sequence: 0,
            commands: 0,
            _affinity: affinity,
        };
        if let Some(assets) = &cohort.input.assets {
            for name in ["input-manifest.json", "assets.json"] {
                cohort
                    .artifacts
                    .insert(assets.join(name), digest(&assets.join(name))?);
            }
        }
        if backends.iter().any(|backend| backend.pvisor()) {
            cohort.preserve_listener()?;
            cohort
                .artifacts
                .insert(cohort.input.binary.clone(), digest(&cohort.input.binary)?);
        }
        if backends.iter().any(|backend| backend.guest()) {
            let receipt = PathBuf::from(
                env::var_os("PVISOR_BENCHMARK_KERNEL_RECEIPT")
                    .context("stock kernel receipt required: PVISOR_BENCHMARK_KERNEL_RECEIPT")?,
            )
            .canonicalize()?;
            let kernels = evidence::verify_kernel(&receipt)?;
            cohort.input.kernel = kernels.get("kernel").cloned();
            cohort.input.vmlinuz = kernels.get("vmlinuz").cloned();
            cohort.input.initrd = kernels.get("initrd").cloned();
            if backends
                .iter()
                .any(|backend| matches!(backend, Backend::Qemu | Backend::QemuMicrovm))
            {
                ensure!(
                    cohort.input.initrd.is_some(),
                    "QEMU templates require prepared stock initrd"
                );
            }
            let cp = resolve("cp")?;
            cohort.artifacts.insert(cp.clone(), digest(&cp)?);
            cohort.input.programs.insert("cp".into(), cp);
            for path in kernels.values().chain(std::iter::once(&receipt)) {
                cohort.artifacts.insert(path.clone(), digest(path)?);
            }
            save(cohort.output.join("kernel-receipt.json"), &read(receipt)?)?;
        }
        if backends.contains(&Backend::Docker) {
            let image = if let Ok(value) = env::var("PVISOR_BENCHMARK_DOCKER_IMAGE") {
                value
            } else {
                read(cohort.input.assets.as_ref().unwrap().join("assets.json"))?["docker_image"]
                    .as_str()
                    .context("assets lack immutable docker_image")?
                    .to_string()
            };
            ensure!(
                image.len() == 71
                    && image.starts_with("sha256:")
                    && image[7..].bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Docker requires immutable image ID"
            );
            let actual = cohort.command(
                &["docker", "inspect", "--format", "{{.Id}}", &image],
                Duration::from_secs(20),
            )?;
            ensure!(
                actual.trim() == image,
                "prepared Docker image identity differs"
            );
            cohort.input.image = Some(image);
        }
        for backend in backends {
            let executable = match backend {
                Backend::Native | Backend::PvisorHost | Backend::PvisorStaged => Some("/bin/sh"),
                Backend::Docker => Some("docker"),
                Backend::FcSystem => Some("firecracker"),
                Backend::Qemu | Backend::QemuMicrovm => Some("qemu-system-x86_64"),
                _ => None,
            };
            if let Some(program) = executable {
                let path = if let Some(path) = cohort.input.programs.get(program) {
                    path.clone()
                } else {
                    resolve(program)?
                };
                cohort.artifacts.insert(path.clone(), digest(&path)?);
                cohort.input.programs.insert(program.into(), path);
            }
        }
        let harness = cohort.output.join("harness");
        fs::create_dir(&harness)?;
        for (name, source) in [
            ("mod.rs", include_str!("mod.rs")),
            ("observer.rs", include_str!("observer.rs")),
            ("subjects.rs", include_str!("subjects.rs")),
            ("evidence.rs", include_str!("evidence.rs")),
            ("prepared.rs", include_str!("prepared.rs")),
        ] {
            let path = harness.join(name);
            fs::write(&path, source)?;
            cohort.artifacts.insert(path.clone(), digest(&path)?);
        }
        let observer_binary = env::current_exe()?.canonicalize()?;
        cohort
            .artifacts
            .insert(observer_binary.clone(), digest(&observer_binary)?);
        let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")?;
        save(
            cohort.output.join("manifest.json"),
            &json!({
                "schema":"pvisor-startup-criterion/v1","benchmark":"B-STARTUP",
                "engine":"Rust Instant; Criterion 0.7.0","backends":backends,
                "sampling":"Criterion adaptive; 60 statistical samples, each containing >=1 checked fresh invocation",
                "minimum_protocol_warmups":3,"cpu_affinity":cohort.input.cpus,
                "metric_scope":"spawn to exact Ready line and same launch to normal zero exit",
                "criterion_metrics":"Ready and normal Exit, separate fresh-invocation streams; both clocks retained for every invocation",
                "cache":"warm host page cache; no global eviction",
                "guest_cpus":2,"guest_memory_mib":128,
                "host_kernel":kernel.trim(),"observer_binary":observer_binary,"resolved_programs":cohort.input.programs,
                "source_build_receipt":"not supplied; binary digest does not prove a source build",
                "input_manifest_sha256":cohort.input_digest,"artifacts":cohort.artifacts,
                "historical_compatibility":"new Rust observation cohort; never append to Python or hyperfine cohorts"
            }),
        )?;
        save(
            cohort.output.join("status.json"),
            &json!({"status":"prepared"}),
        )?;
        Ok(cohort)
    }

    /// Execute untimed setup/cleanup with bounded lifetime and retained logs.
    fn command(&mut self, argv: &[&str], timeout: Duration) -> Result<String> {
        ensure!(!argv.is_empty(), "empty setup/cleanup command");
        let program = if let Some(path) = self.input.programs.get(argv[0]) {
            path.clone()
        } else {
            resolve(argv[0])?
        };
        let hash = digest(&program)?;
        if let Some(expected) = self.artifacts.get(&program) {
            ensure!(*expected == hash, "setup/cleanup executable changed");
        } else {
            self.artifacts.insert(program.clone(), hash);
        }
        self.input.programs.insert(argv[0].into(), program.clone());
        let mut actual = argv
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        actual[0] = program
            .to_str()
            .context("executable path UTF8")?
            .to_string();
        let directory = self.output.join(format!("command-{:06}", self.commands));
        self.commands += 1;
        fs::create_dir(&directory)?;
        save(directory.join("argv.json"), &actual)?;
        let stdout = File::create(directory.join("stdout"))?;
        let stderr = File::create(directory.join("stderr"))?;
        let process = ChildGroup(
            Command::new(&actual[0])
                .args(&actual[1..])
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .process_group(0)
                .spawn()?,
        );
        let start = Instant::now();
        let status = loop {
            if let Some(status) = observer::exited(&process.0)? {
                break status;
            }
            if start.elapsed() >= timeout {
                save(directory.join("exit.json"), &json!({"timeout":true}))?;
                bail!("setup/cleanup command timed out: {argv:?}");
            }
            thread::sleep(Duration::from_millis(2));
        };
        save(
            directory.join("exit.json"),
            &json!({"exit_code":status.code(),"success":status.success()}),
        )?;
        ensure!(
            status.success(),
            "setup/cleanup command failed: {argv:?}; see {}",
            directory.display()
        );
        Ok(fs::read_to_string(directory.join("stdout"))?)
    }

    fn preserve_listener(&self) -> Result<()> {
        let path = PathBuf::from(format!("/tmp/pvisor-host-{}/generation", unsafe {
            libc::geteuid()
        }));
        if !path.exists() {
            return Ok(());
        }
        let generation = read(path)?;
        let pid = generation["pid"]
            .as_i64()
            .context("existing Host listener cannot be verified")?;
        ensure!(
            pid > 0 && pid <= i32::MAX as i64,
            "invalid existing Host listener PID"
        );
        if unsafe { libc::kill(pid as i32, 0) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(error.into());
        }
        let running = PathBuf::from(format!("/proc/{pid}/exe"));
        ensure!(
            digest(&running)? == digest(&self.input.binary)?,
            "selected pVisor differs from live Host listener; use paired binary or private namespace"
        );
        Ok(())
    }

    fn check_sources(&self) -> Result<()> {
        for (path, expected) in &self.artifacts {
            ensure!(
                digest(path)? == *expected,
                "frozen artifact changed: {}",
                path.display()
            );
        }
        if let Some(assets) = &self.input.assets {
            ensure!(
                Some(evidence::verify_inputs(assets)?) == self.input_digest,
                "reference inputs changed"
            );
        }
        Ok(())
    }

    fn prepare_trial(&mut self, backend: Backend, directory: &Path) -> Result<()> {
        fs::create_dir(directory.join("workspace"))?;
        fs::create_dir(directory.join("runs"))?;
        if let Some(assets) = &self.input.assets {
            let source = format!("{}/rootfs/work/.", assets.display());
            self.command(
                &[
                    "cp",
                    "-a",
                    "--reflink=auto",
                    &source,
                    directory
                        .join("workspace")
                        .to_str()
                        .context("workspace UTF8")?,
                ],
                Duration::from_secs(60),
            )?;
        }
        fs::write(directory.join("workspace/startup-canary"), CANARY)?;
        if backend.guest() {
            let source = self.input.assets.as_ref().unwrap().join("agent-env.ext4");
            self.command(
                &[
                    "cp",
                    "--reflink=auto",
                    source.to_str().context("asset UTF8")?,
                    directory
                        .join("rootfs.ext4")
                        .to_str()
                        .context("trial UTF8")?,
                ],
                Duration::from_secs(60),
            )?;
        }
        if backend == Backend::FcSystem {
            File::create(directory.join("firecracker.log"))?;
            let mut boot =
                json!({"kernel_image_path":self.input.kernel,"boot_args":subjects::BOOT});
            if let Some(path) = &self.input.initrd {
                boot["initrd_path"] = json!(path);
            }
            save(
                directory.join("firecracker.json"),
                &json!({
                    "boot-source":boot,
                    "drives":[{"drive_id":"rootfs","path_on_host":directory.join("rootfs.ext4"),
                        "is_root_device":true,"is_read_only":false}],
                    "machine-config":{"vcpu_count":2,"mem_size_mib":128},
                    "logger":{"log_path":directory.join("firecracker.log"),"level":"Warning",
                        "show_level":true,"show_log_origin":true}
                }),
            )?;
        }
        Ok(())
    }

    fn validate(&self, backend: Backend, directory: &Path) -> Result<()> {
        let stdout = String::from_utf8_lossy(&fs::read(directory.join("stdout"))?).into_owned();
        let mut stderr = String::from_utf8_lossy(&fs::read(directory.join("stderr"))?).into_owned();
        if backend == Backend::FcSystem {
            stderr.push_str(&fs::read_to_string(directory.join("firecracker.log"))?);
        }
        evidence::validate_output(&stdout, &stderr, backend.guest())?;
        ensure!(
            fs::read(directory.join("workspace/startup-canary"))? == CANARY,
            "host canary changed"
        );
        if backend.pvisor() {
            let bundles = if backend.staged() {
                vec![directory.join("stage/run-bundle.json")]
            } else {
                evidence::files(&directory.join("runs"))?
                    .into_iter()
                    .filter(|path| {
                        path.file_name()
                            .is_some_and(|name| name == "run-bundle.json")
                    })
                    .collect()
            };
            ensure!(
                bundles.len() == 1 && bundles[0].is_file(),
                "expected exactly one Run Bundle"
            );
            let bundle = read(&bundles[0])?;
            let isolation = if backend == Backend::PvisorVm {
                "virtual_machine"
            } else {
                "rootless_process"
            };
            ensure!(
                bundle["run"]["state"] == "completed"
                    && bundle["run"]["exit_code"] == 0
                    && bundle["run"]["executor"]["isolation"] == isolation,
                "wrong completed Run or isolation"
            );
            ensure!(
                bundle["safety"]["filesystem_changes_staged"] == backend.staged(),
                "wrong staging observation"
            );
            if backend.staged() {
                evidence::require_empty_upper(&directory.join("stage/upper"))?;
            }
        }
        Ok(())
    }

    fn cleanup_docker(&mut self, directory: &Path) -> Result<()> {
        let name = subjects::container_name(&self.input, directory);
        let listed = self.command(
            &[
                "docker",
                "ps",
                "--all",
                "--filter",
                &format!("name=^{name}$"),
                "--format",
                "{{.ID}}",
            ],
            Duration::from_secs(10),
        )?;
        if listed.trim().is_empty() {
            return Ok(());
        }
        let label = self.command(
            &[
                "docker",
                "inspect",
                "--format",
                "{{index .Config.Labels \"pvisor.startup.batch\"}}",
                &name,
            ],
            Duration::from_secs(10),
        )?;
        ensure!(
            label.trim() == self.input.batch,
            "refuse removal of unowned Docker container"
        );
        self.command(&["docker", "rm", "--force", &name], Duration::from_secs(10))?;
        let remaining = self.command(
            &[
                "docker",
                "ps",
                "--all",
                "--filter",
                &format!("name=^{name}$"),
                "--format",
                "{{.ID}}",
            ],
            Duration::from_secs(10),
        )?;
        ensure!(
            remaining.trim().is_empty(),
            "owned container survived cleanup"
        );
        Ok(())
    }

    pub(super) fn trial(&mut self, backend: Backend, phase: &str) -> Result<Observation> {
        self.check_sources()?;
        let directory = self
            .output
            .join(format!("trial-{:08}-{}", self.sequence, backend.name()));
        self.sequence += 1;
        fs::create_dir(&directory)?;
        save(
            directory.join("condition.json"),
            &json!({"backend":backend,"phase":phase,"validated":false}),
        )?;
        let result = (|| {
            self.prepare_trial(backend, &directory)?;
            let argv = subjects::argv(backend, &self.input, &directory)?;
            let env = BTreeMap::from([
                (
                    "PVISOR_RUN_HOME".into(),
                    directory.join("runs").to_string_lossy().into_owned(),
                ),
                ("PVISOR_STARTUP_TIMING".into(), "0".into()),
                ("PVISOR_FS_PROFILE".into(), "0".into()),
            ]);
            let observation = observer::capture(
                &argv,
                &directory.join("workspace"),
                &env,
                &directory,
                b"REFERENCE_READY",
                Duration::from_secs(30),
            )?;
            self.validate(backend, &directory)?;
            self.check_sources()?;
            Ok(observation)
        })();
        // Always perform owned external-resource cleanup, even after a failed
        // observer, fixture preparation or validation. Preserve both failures.
        let cleanup = if backend == Backend::Docker {
            self.cleanup_docker(&directory)
        } else {
            Ok(())
        };
        if let Err(error) = &cleanup {
            save(
                directory.join("cleanup-failure.json"),
                &json!({"error":format!("{error:#}")}),
            )?;
        }
        let result = match (result, cleanup) {
            (Ok(observation), Ok(())) => Ok(observation),
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error),
        };
        match &result {
            Ok(_) => {
                save(
                    directory.join("condition.json"),
                    &json!({"backend":backend,"phase":phase,"validated":true,
                    "stdout_sha256":digest(&directory.join("stdout"))?,
                    "stderr_sha256":digest(&directory.join("stderr"))?}),
                )?;
                if backend.guest() {
                    // Only a fully checked disk is disposable. Failed disks stay
                    // beside their logs for diagnosis.
                    fs::remove_file(directory.join("rootfs.ext4"))?;
                }
            }
            Err(error) => {
                save(
                    directory.join("failure.json"),
                    &json!({"error":format!("{error:#}")}),
                )?;
                self.fail(&format!("{error:#}"))?;
            }
        }
        result
    }

    pub(super) fn fail(&self, error: &str) -> Result<()> {
        save(
            self.output.join("status.json"),
            &json!({"status":"failed","error":error,"trials":self.sequence}),
        )
    }

    pub(super) fn trial_index(&self) -> u64 {
        self.sequence
    }

    pub(super) fn record(&self, filename: &str, value: &serde_json::Value) -> Result<()> {
        save(self.output.join(filename), value)
    }

    pub(super) fn finish(&self) -> Result<()> {
        self.check_sources()?;
        save(
            self.output.join("status.json"),
            &json!({"status":"complete","trials":self.sequence}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_trial_is_fresh_checked_and_retained() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join(".data/native");
        let mut cohort = Cohort::new(output.clone(), &[Backend::Native]).unwrap();
        let a = cohort.trial(Backend::Native, "fixture").unwrap();
        let b = cohort.trial(Backend::Native, "fixture").unwrap();
        assert!(a.ready_ns > 0 && a.ready_ns <= a.exit_ns);
        assert!(b.ready_ns > 0 && b.ready_ns <= b.exit_ns);
        for index in 0..2 {
            let trial = output.join(format!("trial-{index:08}-native"));
            assert_eq!(
                read(trial.join("condition.json")).unwrap()["validated"],
                true
            );
            assert_eq!(
                fs::read(trial.join("workspace/startup-canary")).unwrap(),
                CANARY
            );
            assert!(trial.join("timing.json").is_file());
        }
        cohort.finish().unwrap();
        assert!(Cohort::new(output, &[Backend::Native]).is_err());
    }

    #[test]
    fn evidence_rejects_output_outside_data() {
        let root = tempfile::tempdir().unwrap();
        assert!(Cohort::new(root.path().join("unretained"), &[Backend::Native]).is_err());
    }
    #[test]
    fn evidence_rejects_data_symlink_escape_before_creating_cohort() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("outside")).unwrap();
        std::os::unix::fs::symlink(root.path().join("outside"), root.path().join(".data")).unwrap();
        assert!(Cohort::new(root.path().join(".data/new"), &[Backend::Native]).is_err());
        assert!(!root.path().join("outside/new").exists());
    }
}

//! B-LAZY-STARTUP prepared same-source OCI/TurboOCI/container versus VM startup.
use super::{
    lab::Lab,
    process::{self, load, save, strings},
    source::{self, Config},
    vm::Vm,
};
use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub(super) const GROUPS: [&str; 3] = ["oci", "turbo", "pvisor"];
pub(super) struct Cohort {
    pub config: Config,
    pub lab: Lab,
    pub vm: Vm,
    pub schedule: process::Schedule,
    pub block: u64,
    pub records: Vec<Value>,
}
impl Cohort {
    pub(super) fn prepare(scope: &str) -> Result<Self> {
        let config = Config::from_env(scope)?;
        let result = (|| -> Result<(Lab, Vm)> {
            let lab = Lab::new(
                &config.lab,
                &config.output.join("lab"),
                &config.cpus,
                &config.service_cpus,
            )?;
            let vm = Vm::prepare(&config)?;
            Ok((lab, vm))
        })();
        let (lab, vm) = match result {
            Ok(v) => v,
            Err(e) => {
                save(
                    &config.output.join("failure.json"),
                    &json!({"valid":false,"error":format!("{e:#}")}),
                )?;
                return Err(e);
            }
        };
        let mut this = Self {
            config,
            lab,
            vm,
            schedule: process::Schedule::new(3, 20261018),
            block: 0,
            records: Vec::new(),
        };
        let preparation = (|| -> Result<()> {
            fs::create_dir(this.config.output.join("trials"))?;
            fs::create_dir(this.config.output.join("pairs"))?;
            // Reacquire live budget proof for this cohort, never trust a
            // previous receipt or a `valid` flag supplied by a caller.
            for group in ["oci", "turbo"] {
                this.container_preflight(group)?;
            }
            freeze_harness(&this.config.output)?;
            process::pin(&this.config.cpus)?;
            Ok(())
        })();
        if let Err(error) = preparation {
            let teardown = this.vm.stop();
            save(
                &this.config.output.join("status.json"),
                &json!({
                    "valid":false,"status":"prepare-failed","error":format!("{error:#}"),
                    "teardown_error":teardown.err().map(|e|format!("{e:#}"))
                }),
            )?;
            return Err(error);
        }
        Ok(this)
    }
    fn container_preflight(&mut self, group: &str) -> Result<()> {
        let directory = self.config.output.join(format!("prepare-{group}"));
        fs::create_dir(&directory)?;
        let (snapshotter, image) = self.image(group);
        self.pull(group, &snapshotter, &image)?;
        let id = format!("image-preflight-{}", &process::nonce()?[..16]);
        let mut argv = self.run_argv(&id, &snapshotter, &image);
        argv.extend(strings(&[
            "/bin/sh",
            "-ec",
            &format!("{}; sleep 2", source::WORKLOAD),
        ]));
        let mut command = process::clean_command(&argv)?;
        let leaf = format!("/sys/fs/cgroup{}/{}", self.lab.prefix, id);
        let mut checked = false;
        let mut check = || -> Result<()> {
            let script = format!(
                r#"set -eu
leaf=$1
for attr in memory.max cpu.max cgroup.procs cgroup.events cpuset.cpus.effective; do
 printf '\n=== %s ===\n' "$attr"; cat "$leaf/$attr"
done
printf '\n=== tcmu-targets ===\n'
for target in /sys/kernel/config/target/core/user_*/*; do test -f "$target/info" || continue; printf '%s\n' "$target"; cat "$target/info"; done
printf '\n=== mountinfo ===\n';cat /proc/self/mountinfo
printf '\n=== container-info ===\n'
{} --address {} --namespace "$2" containers info "$3"
"#,
                super::lab::CTR,
                super::lab::SOCKET
            );
            let namespace = self.lab.manifest["namespace"].as_str().unwrap().to_owned();
            let raw = self.lab.docker_command(&strings(&[
                "sh", "-ec", &script, "sh", &leaf, &namespace, &id,
            ]))?;
            fs::write(directory.join("live-budgets.txt"), &raw)?;
            validate_live(&raw, &snapshotter, &image, &self.config.cpus)?;
            checked = true;
            Ok(())
        };
        let observed = process::observe(
            &mut command,
            &directory.join("task"),
            Some(source::MARKER),
            Duration::from_secs(90),
            Some(&mut check),
        );
        drop(check);
        let cleanup = if observed.is_ok() {
            self.lab.cleanup(&id, &directory)
        } else {
            recover(&mut self.lab, &id, &directory)
        };
        observed?;
        cleanup?;
        ensure!(checked, "live budget observation missing");
        save(
            &directory.join("result.json"),
            &json!({"valid":true,"action":"prepare","condition":group,"budgets_verified":true,
            "cpus":self.config.cpus,"memory_bytes":2147483648u64,"cpu_quota":2,"workload_argv":source::workload(),
            "provenance":self.lab.provenance,"boot_id":self.lab.baseline["boot_id"]}),
        )?;
        Ok(())
    }
    fn image(&self, group: &str) -> (String, String) {
        if group == "turbo" {
            ("overlaybd".into(), self.lab.derived_reference.clone())
        } else {
            ("overlayfs".into(), self.lab.source_reference.clone())
        }
    }
    fn pull(&mut self, group: &str, snapshotter: &str, image: &str) -> Result<()> {
        let mut args = if group == "turbo" {
            strings(&["rpull"])
        } else {
            strings(&["images", "pull"])
        };
        args.extend(strings(&[
            "--plain-http",
            "--snapshotter",
            snapshotter,
            image,
        ]));
        self.lab.ctr_command(&args)?;
        Ok(())
    }
    fn run_argv(&self, id: &str, snapshotter: &str, image: &str) -> Vec<String> {
        let mut argv = self.lab.ctr.clone();
        argv.extend(strings(&[
            "run",
            "--rm",
            "--snapshotter",
            snapshotter,
            "--cpus",
            "2",
            "--cpuset-cpus",
            &self.lab.cpus,
            "--memory-limit",
            "2147483648",
            "--cgroup",
            &format!("{}/{}", self.lab.prefix, id),
            image,
            id,
        ]));
        argv
    }
    /// One fresh task/VM. Reset/pull/preparation/validation are never included
    /// in its stored monotonic process interval.
    pub(super) fn trial(
        &mut self,
        group: &str,
        id: &str,
        lazy: bool,
        warm: bool,
        key: &str,
    ) -> Result<Value> {
        if group == "pvisor" {
            return self.vm.trial(id, lazy, key, warm);
        }
        let directory = self.config.output.join("trials").join(id);
        fs::create_dir(&directory)?;
        let (snapshotter, image) = self.image(group);
        let task = format!("image-{}", &process::nonce()?[..24]);
        fs::write(
            directory.join("recovery.txt"),
            format!(
                "Owned task {task}, private namespace {}, container {}. Timeout only kills local Docker client. Retain lab if scoped recovery/inventory fails; never delete global devices.\n",
                self.lab.manifest["namespace"], self.lab.container
            ),
        )?;
        let mut run = self.run_argv(&task, &snapshotter, &image);
        run.extend(source::workload());
        let argv = if lazy {
            let mut pull = self.lab.ctr[3..].to_vec();
            pull.extend(if group == "turbo" {
                strings(&["rpull"])
            } else {
                strings(&["images", "pull"])
            });
            pull.extend(strings(&[
                "--plain-http",
                "--snapshotter",
                &snapshotter,
                &image,
            ]));
            let script = format!(
                "{} 1>&2\nexec {}",
                process::shell(&pull),
                process::shell(&run[3..])
            );
            let mut argv = self.lab.docker.clone();
            argv.extend(strings(&[
                "/lab/tools/taskset",
                "-c",
                &self.config.cpu_text(),
                "/bin/sh",
                "-ec",
                &script,
            ]));
            argv
        } else {
            self.pull(group, &snapshotter, &image)?;
            run
        };
        let cursor = self.lab.registry_cursor()?;
        save(&directory.join("host-before.json"), &self.lab.baseline)?;
        let observed = process::observe(
            &mut process::clean_command(&argv)?,
            &directory.join("timed"),
            Some(source::MARKER),
            Duration::from_secs(300),
            None,
        );
        let cleanup = if observed.is_ok() {
            self.lab.cleanup(&task, &directory)
        } else {
            recover(&mut self.lab, &task, &directory)
        };
        let result = (|| -> Result<Value> {
            let observed = observed?;
            cleanup?;
            let traffic = self.lab.registry_observation(cursor, &directory)?;
            let registry = load(&directory.join("registry-responses.json"))?;
            if lazy {
                let blob = registry["blob_body_bytes"]
                    .as_u64()
                    .context("missing blob bytes")?;
                ensure!(
                    (!warm && blob > 0) || (warm && blob == 0),
                    "cold/hot registry blob gate failed"
                );
                ensure!(
                    (!warm && traffic.content_bytes > 0) || (warm && traffic.content_bytes == 0),
                    "cold/hot content gate failed"
                );
            }
            self.lab.check_artifacts()?;
            Ok(
                json!({"valid":true,"condition":group,"cache_state":if lazy{if warm{"warm"}else{"cold"}}else{"prepared"},
                "task_id":task,"ready_ns":observed.timing.ready_ns,"completion_ns":observed.timing.completion_ns,
                "timing":observed.timing,"traffic":traffic,"registry":registry,"workload_argv":source::workload(),
                "timed_argv":argv,"registry_log_sha256":source::digest(&directory.join("registry-responses.log"))?,
                "provenance":self.lab.provenance,"evidence_dir":directory,"snapshotter":snapshotter,"image":image,
                "stdout_sha256":source::bytes_digest(&observed.stdout),"stderr_sha256":source::bytes_digest(&observed.stderr)}),
            )
        })();
        match &result {
            Ok(v) => save(&directory.join("result.json"), v)?,
            Err(e) => save(
                &directory.join("result.json"),
                &json!({"valid":false,"error":format!("{e:#}"),"task_id":task}),
            )?,
        }
        result
    }
    pub(super) fn record(
        &mut self,
        phase: &str,
        index: u64,
        group: &str,
        state: &str,
        result: Value,
    ) -> Result<u64> {
        ensure!(result["valid"] == true, "invalid measured trial");
        let ready = result["ready_ns"].as_u64().context("missing ready")?;
        let completion = result["completion_ns"]
            .as_u64()
            .context("missing completion")?;
        ensure!(
            ready > 0 && completion >= ready,
            "invalid measured clock order"
        );
        self.records.push(
            json!({"phase":phase,"block":index,"condition":group,"state":state,"result":result}),
        );
        save(
            &self.config.output.join("records.partial.json"),
            &self.records,
        )?;
        Ok(ready)
    }
    pub(super) fn finish(&mut self, outcome: &Result<()>) -> Result<()> {
        let stop = self.vm.stop();
        let audit = self.lab.check_artifacts();
        let directory = self.config.output.join("final-audit");
        fs::create_dir(&directory)?;
        let inventory = self.lab.audit_inventory(&directory);
        let harness = check_harness(&self.config.output);
        let semantic = super::audit::cohort(
            &self.config.output,
            &self.records,
            load(&self.config.output.join("protocol.json"))?["scope"] == "cold-warm-client",
            self.block,
        );
        let valid = outcome.is_ok()
            && stop.is_ok()
            && audit.is_ok()
            && inventory.is_ok()
            && harness.is_ok()
            && semantic.is_ok();
        save(&self.config.output.join("records.json"), &self.records)?;
        save(
            &self.config.output.join("status.json"),
            &json!({"valid":valid,"status":if valid{"completed"}else{"failed"},
            "failure":outcome.as_ref().err().map(|e|format!("{e:#}")),"teardown":stop.as_ref().err().map(|e|format!("{e:#}")),
            "artifact_audit":audit.as_ref().err().map(|e|format!("{e:#}")),"inventory":inventory.as_ref().err().map(|e|format!("{e:#}")),
            "harness":harness.as_ref().err().map(|e|format!("{e:#}")),
            "semantic_audit":semantic.as_ref().err().map(|e|format!("{e:#}")),"excluded":0}),
        )?;
        stop?;
        audit?;
        inventory?;
        harness?;
        semantic?;
        Ok(())
    }
    fn block(&mut self, phase: &str, metric: &str) -> Result<Duration> {
        let index = self.block;
        self.block += 1;
        let order = self.schedule.next();
        let before = process::snapshot(&[self.vm.pid()])?;
        let mut sum = 0;
        save(
            &self.config.output.join(format!("order-{index:06}.json")),
            &json!({"phase":phase,"block":index,"order":order}),
        )?;
        for condition in order {
            let group = GROUPS[condition];
            let id = format!("{phase}-{index:06}-{group}");
            let result = self.trial(group, &id, false, false, "prepared")?;
            let measured = result[metric]
                .as_u64()
                .context("missing selected process duration")?;
            self.record(phase, index, group, "prepared", result)?;
            sum += measured;
        }
        process::background(
            &before,
            &process::snapshot(&[self.vm.pid()])?,
            &self.config.cpus,
            &self
                .config
                .output
                .join(format!("background-{index:06}.json")),
        )?;
        Ok(Duration::from_nanos(sum))
    }
}
pub(super) fn run(criterion: &mut Criterion) -> Result<()> {
    let mut cohort = Cohort::prepare("warm-prepared")?;
    let outcome = measure(criterion, &mut cohort, false);
    let finish = cohort.finish(&outcome);
    outcome?;
    finish
}

fn measured_block(cohort: &mut Cohort, phase: &str, metric: &str, lazy: bool) -> Result<Duration> {
    if lazy {
        super::image_lazy::block(cohort, phase, metric)
    } else {
        cohort.block(phase, metric)
    }
}

/// Both experiments use the same sampling rule. Each metric stream executes
/// fresh operations and retains both clocks; no duration is replayed from
/// another stream. Catch the infallible Criterion callback's abort so all
/// native owners still receive normal teardown and final evidence auditing.
pub(super) fn measure(criterion: &mut Criterion, cohort: &mut Cohort, lazy: bool) -> Result<()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        measured_block(cohort, "preflight", "ready_ns", lazy)?;
        for _ in 0..3 {
            measured_block(cohort, "protocol-warmup", "ready_ns", lazy)?;
        }
        let family = if lazy {
            "image_lazy_compare"
        } else {
            "image_compare"
        };
        let mut group = criterion.benchmark_group(format!("startup/{family}"));
        group.sample_size(30);
        group.sampling_mode(SamplingMode::Flat);
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(5));
        for metric in ["ready_ns", "completion_ns"] {
            let mut failure = None;
            let mut callback = 0u64;
            let mut spans = Vec::new();
            let phase = format!("criterion-{metric}");
            group.bench_function(format!("randomized-paired-{metric}"), |bencher| {
                bencher.iter_custom(|iterations| {
                    callback += 1;
                    let first = cohort.block;
                    let mut total = Duration::ZERO;
                    for _ in 0..iterations {
                        match measured_block(cohort, &phase, metric, lazy) {
                            Ok(duration) => total += duration,
                            Err(error) => {
                                failure = Some(format!("{error:#}"));
                                break;
                            }
                        }
                    }
                    spans.push((first, cohort.block));
                    let path = cohort
                        .config
                        .output
                        .join(format!("criterion-call-{metric}-{callback:06}.json"));
                    let receipt = json!({
                        "first_block": first,
                        "iterations_requested": iterations,
                        "blocks_completed": cohort.block-first,
                        "returned_ns": total.as_nanos().to_string(),
                        "includes_framework_calibration": true,
                        "valid": failure.is_none(),
                        "metric": metric,
                        "scope": if lazy {"sum selected intervals across three cold/hot pairs"}
                                 else {"sum selected intervals across three prepared conditions"}
                    });
                    if let Err(error) = save(&path, &receipt) {
                        failure = Some(format!("callback evidence write failed: {error:#}"));
                    }
                    if let Some(error) = &failure {
                        std::panic::panic_any(format!("image correctness gate failed: {error}"));
                    }
                    total
                });
            });
            ensure!(failure.is_none(), "image cohort failed");
            record_callbacks(cohort, &spans, metric)?;
        }
        group.finish();
        Ok(())
    }))
    .unwrap_or_else(|payload| {
        let detail = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "callback panicked".into());
        Err(anyhow::anyhow!(
            "Criterion image callback aborted: {detail}; see retained failed trial"
        ))
    })
}

/// Recover only an internally generated task ID in the verified private lab.
pub(super) fn recover(lab: &mut Lab, id: &str, evidence: &Path) -> Result<()> {
    let result = (|| -> Result<()> {
        // IDs are generated internally and namespace ownership was verified.
        let tasks = lab.ctr_command(&strings(&["tasks", "list"]))?;
        if tasks
            .lines()
            .skip(1)
            .any(|l| l.split_whitespace().next() == Some(id))
        {
            lab.ctr_command(&strings(&["tasks", "kill", "--signal", "SIGKILL", id]))?;
            lab.ctr_command(&strings(&["tasks", "delete", "--force", id]))?;
        }
        let containers = lab.ctr_command(&strings(&["containers", "list"]))?;
        if containers
            .lines()
            .skip(1)
            .any(|l| l.split_whitespace().next() == Some(id))
        {
            lab.ctr_command(&strings(&["containers", "delete", id]))?;
        }
        lab.cleanup(id, evidence)
    })();
    save(
        &evidence.join("recovery-result.json"),
        &json!({"valid":result.is_ok(),"task_id":id,"error":result.as_ref().err().map(|e|format!("{e:#}"))}),
    )?;
    result
}
fn validate_live(raw: &str, snapshotter: &str, image: &str, cpus: &[usize]) -> Result<()> {
    let mut sections = std::collections::BTreeMap::<String, String>::new();
    let mut current = String::new();
    for line in raw.lines() {
        if line.starts_with("=== ") && line.ends_with(" ===") {
            current = line[4..line.len() - 4].into();
            sections.insert(current.clone(), String::new());
        } else if !current.is_empty() {
            sections
                .get_mut(&current)
                .unwrap()
                .push_str(&format!("{line}\n"));
        }
    }
    let text = |name: &str| -> Result<&str> {
        Ok(sections.get(name).context("missing live section")?.trim())
    };
    ensure!(
        text("memory.max")? == "2147483648",
        "live memory limit not 2 GiB"
    );
    let quota: Vec<_> = text("cpu.max")?.split_whitespace().collect();
    ensure!(
        quota.len() == 2
            && quota[0].parse::<u64>()? == 2 * quota[1].parse::<u64>()?
            && quota[1].parse::<u64>()? > 0,
        "live CPU quota not two CPUs"
    );
    ensure!(
        text("cgroup.procs")?.split_whitespace().next().is_some()
            && text("cgroup.procs")?
                .split_whitespace()
                .all(|p| p.parse::<u32>().is_ok()),
        "missing live cgroup member"
    );
    ensure!(
        process::cpus(text("cpuset.cpus.effective")?)? == cpus,
        "live cpuset differs"
    );
    let info: Value = serde_json::from_str(text("container-info")?)?;
    ensure!(
        info["Snapshotter"] == snapshotter && info["Image"] == image,
        "actual image/snapshotter differs"
    );
    if snapshotter == "overlaybd" {
        ensure!(
            text("tcmu-targets")?.contains("/sys/kernel/config/target/core/user_"),
            "OverlayBD has no actual TCMU target"
        );
    }
    Ok(())
}
fn freeze_harness(output: &Path) -> Result<()> {
    let dir = output.join("harness");
    fs::create_dir(&dir)?;
    for (name, body) in [
        ("mod.rs", include_str!("mod.rs")),
        ("image_compare.rs", include_str!("image_compare.rs")),
        ("image_lazy.rs", include_str!("image_lazy.rs")),
        ("lab.rs", include_str!("lab.rs")),
        ("process.rs", include_str!("process.rs")),
        ("source.rs", include_str!("source.rs")),
        ("proxy.rs", include_str!("proxy.rs")),
        ("vm.rs", include_str!("vm.rs")),
        ("audit.rs", include_str!("audit.rs")),
    ] {
        fs::write(dir.join(name), body)?;
    }
    save(&output.join("harness-manifest.json"), &source::tree(&dir)?)?;
    save(
        &output.join("observer-binary.json"),
        &json!({"path":std::env::current_exe()?,"sha256":source::digest(&std::env::current_exe()?)?,
        "protocol_sha256":source::digest(&output.join("protocol.json"))?,
        "relationship":"compiled harness source embedded at build time; product binary build correspondence unverified"}),
    )?;
    Ok(())
}
fn check_harness(output: &Path) -> Result<()> {
    ensure!(
        source::tree(&output.join("harness"))? == load(&output.join("harness-manifest.json"))?,
        "frozen harness changed"
    );
    let record = load(&output.join("observer-binary.json"))?;
    ensure!(
        source::digest(&output.join("protocol.json"))?
            == record["protocol_sha256"]
                .as_str()
                .context("protocol hash missing")?,
        "cohort protocol changed"
    );
    ensure!(
        source::digest(Path::new(record["path"].as_str().unwrap()))?
            == record["sha256"].as_str().unwrap(),
        "observer executable changed"
    );
    Ok(())
}
pub(super) fn record_callbacks(cohort: &Cohort, spans: &[(u64, u64)], metric: &str) -> Result<()> {
    let ledger: Vec<_> = spans
        .iter()
        .enumerate()
        .map(|(i, (begin, end))| {
            json!({
                "callback":i+1,"first_block":begin,"last_block_exclusive":end,
                "phase":"framework-unspecified",
            })
        })
        .collect();
    let output = cohort.config.output.join(format!("metric-{metric}"));
    fs::create_dir(&output)?;
    save(
        &output.join("criterion-callbacks.json"),
        &json!({"criterion_version":"0.7.0",
        "sampling_mode":"Flat","callbacks":ledger,"metric":metric,
        "phase_boundary":"Criterion's public iter_custom API does not identify warmup/calibration/measurement",
        "includes_framework_calibration":true,
        "inference_owner":"Criterion artifacts only; callback ranges are not an authoritative formal sample map",
        "publication_gate":"unsatisfied: authoritative paired statistical analysis is not supplied by these callbacks",
        "stream":"fresh operations; never replays durations from another metric stream"}),
    )?;
    Ok(())
}

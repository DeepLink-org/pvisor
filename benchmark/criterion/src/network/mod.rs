//! B-NETWORK-PROFILE-ENG: randomized fresh-VM profile comparisons.
//! Criterion's timed operation is one complete paired cohort. Worker clocks
//! remain the network metrics; outer wall time is never called network latency.
#[cfg(target_os = "linux")]
mod linux {
    use crate::filesystem::support::*;
    use anyhow::{Context, Result, ensure};
    use criterion::Criterion;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        fs::{self, File},
        io::{BufRead, BufReader},
        net::{IpAddr, ToSocketAddrs},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::Duration,
    };
    const SCENARIOS: [&str; 11] = [
        "connect",
        "rpc_fresh",
        "rpc_keepalive",
        "dns",
        "stream",
        "download_1",
        "upload_1",
        "download_8",
        "upload_8",
        "download_32",
        "upload_32",
    ];
    const ARMS: [&str; 3] = ["native", "release", "net_o3"];
    const BUILDERS: [&str; 3] = ["cargo", "rustc", "cargo-zigbuild"];

    fn parse(text: &str, arm: &str, order: &[&str]) -> Result<Vec<Value>> {
        let ready = unique(text, "NET_READY ")?;
        ensure!(
            ready["kind"] == if arm == "native" { "native" } else { "vm" },
            "wrong client ready"
        );
        ensure!(
            text.lines().filter(|l| *l == "NET_COMPLETE").count() == 1,
            "completion marker"
        );
        let records = records(text, "NET_RESULT ")?;
        let expected: Vec<_> = order
            .iter()
            .filter(|s| !(arm == "native" && **s == "dns"))
            .copied()
            .collect();
        let names: Vec<_> = records
            .iter()
            .map(|r| r["scenario"].as_str().unwrap_or(""))
            .collect();
        ensure!(
            names == expected,
            "incomplete, duplicate, or reordered scenarios"
        );
        for row in &records {
            let name = row["scenario"].as_str().context("scenario")?;
            ensure!(
                row["elapsed_ns"].as_u64().unwrap_or(0) > 0,
                "nonpositive clock"
            );
            let (ops, bytes, samples) = match name {
                "connect" => (128, 0, 128),
                "rpc_fresh" => (128, 128 * 2048, 128),
                "rpc_keepalive" => (256, 256 * 2048, 256),
                "dns" => (128, 0, 128),
                "stream" => (32, 2048, 31),
                _ => {
                    let n = name.rsplit('_').next().unwrap().parse::<u64>()?;
                    (n, 64 << 20, 0)
                }
            };
            ensure!(
                row["operations"] == ops && row["bytes"] == bytes,
                "wrong operations/byte count"
            );
            let clocks = row["samples_ns"].as_array().context("samples_ns")?;
            ensure!(
                clocks.len() == samples && clocks.iter().all(|v| v.as_u64().unwrap_or(0) > 0),
                "per-request clocks"
            );
            if name == "stream" {
                let first = row["first_byte_ns"].as_u64().unwrap_or(0);
                ensure!(
                    first > 0 && first <= row["elapsed_ns"].as_u64().unwrap(),
                    "stream first-byte clock"
                );
            }
        }
        Ok(records)
    }
    fn bundle(path: &Path, port: u16) -> Result<Value> {
        let value = load(path)?;
        ensure!(
            value["run"]["executor"]["kind"] == "virtual_machine"
                && value["run"]["state"] == "completed"
                && value["run"]["exit_code"] == 0
                && value["run"]["metrics"]["resource.vm_memory_bytes"] == 256 << 20
                && value["safety"]["network_non_bypassable"] == true
                && value["network"]["interception"]["driver"] == "vm-smoltcp"
                && value["network"]["interception"]["strength"] == "non-bypassable",
            "VM network boundary"
        );
        let policy = &value["network"]["policy"];
        ensure!(
            policy["mode"] == "allowlist"
                && policy["limits"] == json!([])
                && policy["rules"]
                    == json!([{"host":"localhost.localdomain","ports":[port],"allow_private_ips":true}]),
            "endpoint policy"
        );
        let counters = &value["network"]["intercepted"];
        let expected = json!({"policy_allowed":340,"policy_denied":0,"failures":0,"dns_queries":129,
        "dns_answers":129,"tcp_flows_opened":340,"tcp_flows_denied":0,"tcp_connect_failures":0,
        "bytes_guest_to_host":201723200,"bytes_host_to_guest":201722184,
        "unsupported_packets":0,"active_tcp_flows":0});
        for (key, expected) in expected.as_object().unwrap() {
            ensure!(
                &counters[key] == expected,
                "network counter mismatch: {key}"
            );
        }
        Ok(counters.clone())
    }
    struct Experiment {
        output: PathBuf,
        worker: PathBuf,
        firmware: PathBuf,
        cpus: String,
        server: OwnedChild,
        port: u16,
        rng: Rng,
        index: u64,
        observations: Vec<Value>,
        hashes: Value,
    }
    impl Experiment {
        fn prepare() -> Result<Self> {
            ensure!(
                cfg!(all(target_os = "linux", target_arch = "x86_64")),
                "Linux x86_64/KVM required"
            );
            File::options().read(true).write(true).open("/dev/kvm")?;
            ensure!(
                builders(&BUILDERS)?.is_empty(),
                "external build in progress"
            );
            let addresses: BTreeSet<IpAddr> = ("localhost.localdomain", 80)
                .to_socket_addrs()?
                .map(|a| a.ip())
                .collect();
            ensure!(
                !addresses.is_empty() && addresses.iter().all(IpAddr::is_loopback),
                "localhost.localdomain must resolve exclusively to loopback"
            );
            let output = output()?;
            let frozen = output.join("frozen");
            fs::create_dir(&frozen)?;
            let mut hashes = serde_json::Map::new();
            for (name, env) in [
                ("release", "PVISOR_BENCH_BASELINE"),
                ("net_o3", "PVISOR_BENCH_CANDIDATE"),
                ("worker", "PVISOR_BENCH_NET_WORKER"),
            ] {
                let source = required(env)?;
                let target = frozen.join(name);
                fs::copy(source, &target)?;
                hashes.insert(format!("frozen/{name}"), json!(sha(&target)?));
            }
            let firmware_source = required("PVISOR_BENCH_FIRMWARE")?;
            let firmware = frozen.join("firmware");
            fs::create_dir(&firmware)?;
            for entry in fs::read_dir(firmware_source)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("libkrunfw")
                    && entry.path().is_file()
                {
                    let target = firmware.join(entry.file_name());
                    fs::copy(entry.path(), &target)?;
                    hashes.insert(
                        format!("frozen/firmware/{}", entry.file_name().to_string_lossy()),
                        json!(sha(&target)?),
                    );
                }
            }
            ensure!(
                firmware.join("libkrunfw.so.5").is_file(),
                "firmware soname libkrunfw.so.5 required"
            );
            if let Ok(receipt) = std::env::var("PVISOR_BENCH_BUILD_RECEIPT") {
                checked(
                    Command::new("cp")
                        .args(["-a", "--"])
                        .arg(receipt)
                        .arg(frozen.join("build-receipt")),
                )?;
            }
            let cpus = std::env::var("PVISOR_BENCH_CPUS").unwrap_or_else(|_| "0,1,2,3".into());
            let server_cpus =
                std::env::var("PVISOR_BENCH_SERVER_CPUS").unwrap_or_else(|_| "4,5".into());
            let parse_cpus = |s: &str| -> Result<BTreeSet<usize>> {
                s.split(',').map(|v| Ok(v.parse()?)).collect()
            };
            let a = parse_cpus(&cpus)?;
            let b = parse_cpus(&server_cpus)?;
            ensure!(
                a.len() == 4 && b.len() == 2 && a.is_disjoint(&b),
                "four client and two disjoint server CPUs required"
            );
            let worker = frozen.join("worker");
            let mut command = Command::new("taskset");
            command
                .args(["-c", &server_cpus])
                .arg(&worker)
                .arg("server")
                .stdout(Stdio::piped())
                .stderr(File::create(output.join("server.stderr"))?);
            clean(&mut command);
            let mut server = spawn(&mut command)?;
            let stdout = server.take_stdout().unwrap();
            // Readiness is bounded even when the server fails without emitting a newline.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
                let _ = tx.send(result);
            });
            let line = rx
                .recv_timeout(Duration::from_secs(10))
                .context("origin readiness timeout")??;
            let port = line
                .trim()
                .strip_prefix("READY ")
                .context("origin startup failed")?
                .parse()?;
            let hashes = Value::Object(hashes);
            save(
                output.join("manifest.json"),
                &json!({"benchmark":"B-NETWORK-PROFILE-ENG","schema":2,
            "metric_scope":"worker internal network clocks","criterion_operation":"randomized native/release/net_o3 cohort",
            "warmup_cohorts":3,"minimum_formal_cohorts":30,"cpus":cpus,"server_cpus":server_cpus,
            "artifacts":hashes,"resolution":addresses,"source":include_str!("mod.rs"),
            "cpuinfo":fs::read_to_string("/proc/cpuinfo")?,"meminfo":fs::read_to_string("/proc/meminfo")?,
            "artifact_origin_claim":"digests only; build provenance requires separately audited receipt"}),
            )?;
            Ok(Self {
                output,
                worker,
                firmware,
                cpus,
                server,
                port,
                rng: Rng(20261115),
                index: 0,
                observations: vec![],
                hashes,
            })
        }
        fn trial(&mut self, arm: &str, phase: &str, index: u64, order: &[&str]) -> Result<Value> {
            ensure!(self.server.try_status()?.is_none(), "origin server failed");
            let folder = self.output.join(format!("{phase}-{index:08}-{arm}"));
            fs::create_dir(&folder)?;
            let workspace = folder.join("workspace");
            fs::create_dir(&workspace)?;
            let mut command = Command::new("taskset");
            command.args(["-c", &self.cpus]);
            let kind = if arm == "native" { "native" } else { "vm" };
            if arm == "native" {
                command.arg(&self.worker);
            } else {
                let root = folder.join("rootfs");
                fs::create_dir_all(root.join("bench"))?;
                fs::copy(&self.worker, root.join("bench/net-worker"))?;
                command
                    .arg(self.output.join(format!("frozen/{arm}")))
                    .args(["spawn", "--no-agent-defaults", "--vm", "--rootfs"])
                    .arg(root)
                    .arg("--vm-library-dir")
                    .arg(&self.firmware)
                    .args([
                        "--cpu",
                        "2",
                        "--memory",
                        "256MiB",
                        "--stdio",
                        "inherit",
                        "--timeout",
                        "120s",
                        "--stage",
                    ])
                    .arg(folder.join("stage"))
                    .args([
                        "--overlaynet",
                        "auto",
                        "--overlaynet-policy",
                        "allowlist",
                        "--overlaynet-rule",
                    ])
                    .arg(format!(
                        "host = \"localhost.localdomain\", ports = [{}], allow_private_ips = true",
                        self.port
                    ))
                    .args(["--", "/bench/net-worker"]);
            }
            command
                .args(["client", kind, &self.port.to_string(), &order.join(",")])
                .current_dir(workspace);
            clean(&mut command);
            for (env, dir) in [
                ("HOME", "home"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_RUNTIME_DIR", "runtime"),
                ("PVISOR_RUN_HOME", "runs"),
            ] {
                let path = folder.join(dir);
                fs::create_dir(&path)?;
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                command.env(env, path);
            }
            let (wall, text) = run(&mut command, &folder, Duration::from_secs(150), &BUILDERS)?;
            ensure!(self.server.try_status()?.is_none(), "origin server failed");
            let results = parse(&text, arm, order)?;
            let accounting = if arm == "native" {
                Value::Null
            } else {
                bundle(&folder.join("stage/run-bundle.json"), self.port)?
            };
            let value = json!({"phase":phase,"round":index,"arm":arm,"results":results,
            "process_wall_ns":wall.as_nanos(),"network_accounting":accounting});
            save(folder.join("result.json"), &value)?;
            Ok(value)
        }
        fn cohort(
            &mut self,
            phase: &str,
            selected_arm: &str,
            selected_metric: &str,
        ) -> Result<Duration> {
            verify_hashes(&self.output, &self.hashes)?;
            let mut arms = ARMS;
            self.rng.shuffle(&mut arms);
            let mut order = SCENARIOS;
            self.rng.shuffle(&mut order);
            let index = self.index;
            self.index += 1;
            save(
                self.output
                    .join(format!("{phase}-{index:08}-schedule.json")),
                &json!({"arms":arms,"scenarios":order}),
            )?;
            let mut measured = None;
            for arm in arms {
                let result = self.trial(arm, phase, index, &order)?;
                if arm == selected_arm {
                    let key = if selected_metric.starts_with("stream_") {
                        "stream"
                    } else {
                        selected_metric
                    };
                    let row = result["results"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|r| r["scenario"] == key)
                        .context("selected worker metric absent")?;
                    let ns = if selected_metric == "stream_first_byte" {
                        row["first_byte_ns"].as_u64().context("first-byte clock")?
                    } else {
                        row["elapsed_ns"]
                            .as_u64()
                            .context("complete scenario batch clock")?
                    };
                    measured = Some(Duration::from_nanos(ns));
                }
                self.observations.push(result);
                save(
                    self.output.join("completed.json"),
                    &json!(self.observations),
                )?;
            }
            measured.context("selected arm missing")
        }
    }
    /// Register the real-network paired operation. Three explicit checked warmups
    /// precede Criterion calibration; calibration observations remain raw evidence.
    pub fn register(c: &mut Criterion) {
        if std::env::var("PVISOR_BENCH_FAMILY").as_deref() != Ok("network") {
            return;
        }
        let entry =
            std::env::var("PVISOR_BENCH_ENTRY").unwrap_or_else(|_| "network/profile_ab".into());
        assert!(
            matches!(
                entry.as_str(),
                "network/profile_ab" | "network/profile_ab.json" | "profile_ab"
            ),
            "unprepared network entry {entry}"
        );
        let selected = std::env::var("PVISOR_BENCH_METRIC").unwrap_or_else(|_| "rpc_fresh".into());
        assert!(
            SCENARIOS.contains(&selected.as_str()) || selected == "stream_first_byte",
            "unknown network metric"
        );
        let mut experiment = Experiment::prepare().expect("network preparation");
        for _ in 0..3 {
            experiment
                .cohort("explicit-warmup", "release", &selected)
                .expect("network checked warmup");
        }
        let mut group = c.benchmark_group("network/profile_ab");
        group
            .sample_size(30)
            .sampling_mode(criterion::SamplingMode::Flat);
        for arm in ["release", "net_o3"] {
            group.bench_function(format!("{selected}/{arm}/ns-per-batch"), |b| {
                b.iter_custom(|iterations| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iterations {
                        total += experiment
                            .cohort("criterion", arm, &selected)
                            .expect("network paired cohort");
                    }
                    total
                })
            });
        }
        group.finish();
        save(experiment.output.join("validation.json"),&json!({"valid":true,"checked_cohorts":experiment.index,
        "selected_metric":selected,"criterion_owns_statistics":true,
        "raw_observations":"include calibration/warmup; never labelled formal samples",
        "paired_interpretation":"complete randomized pairs retained; Criterion arm intervals are not paired contrast intervals"})).unwrap();
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn worker_text() -> String {
            let mut text = String::from("NET_READY {\"kind\":\"vm\"}\n");
            for name in SCENARIOS {
                let (operations, bytes, samples) = match name {
                    "connect" => (128, 0, 128),
                    "rpc_fresh" => (128, 128 * 2048, 128),
                    "rpc_keepalive" => (256, 256 * 2048, 256),
                    "dns" => (128, 0, 128),
                    "stream" => (32, 2048, 31),
                    _ => (
                        name.rsplit('_').next().unwrap().parse::<u64>().unwrap(),
                        64 << 20,
                        0,
                    ),
                };
                text.push_str(&format!("NET_RESULT {}\n",json!({"scenario":name,"elapsed_ns":10,
                    "operations":operations,"bytes":bytes,"samples_ns":vec![1;samples],"first_byte_ns":2})));
            }
            text.push_str("NET_COMPLETE\n");
            text
        }
        #[test]
        fn exact_worker_inventory_accepts_and_detects_corrupted_work() {
            let text = worker_text();
            assert_eq!(parse(&text, "release", &SCENARIOS).unwrap().len(), 11);
            assert!(
                parse(
                    &text.replace("\"operations\":128", "\"operations\":127"),
                    "release",
                    &SCENARIOS
                )
                .is_err()
            );
            assert!(parse(&format!("{text}NET_COMPLETE\n"), "release", &SCENARIOS).is_err());
        }
        #[test]
        fn rejects_incomplete_worker() {
            assert!(
                parse(
                    "NET_READY {\"kind\":\"vm\"}\nNET_COMPLETE\n",
                    "release",
                    &SCENARIOS
                )
                .is_err()
            );
        }
    }
}

/// Register the Linux/KVM paired protocol, or report its explicit unsupported platform.
pub fn register(c: &mut criterion::Criterion) {
    #[cfg(target_os = "linux")]
    linux::register(c);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = c;
        if std::env::var("PVISOR_BENCH_FAMILY").as_deref() == Ok("network") {
            panic!("network profile protocol requires Linux x86_64/KVM");
        }
    }
}

//! Active STORAGE protocols. Criterion controls warmup, repetition and statistics.
//!
//! Benchmark: B-LAZY-STARTUP (user-facing), B-IMAGE-FORMAT-ENG,
//! B-IMAGE-STORAGE-ENG and B-NATIVE-DOCKER-ENG (engineering A/B).
//! Question: how do same-source image storage, prepared execution/read costs and
//! loopback remote cold/warm supply differ? Each Criterion operation performs an
//! actual randomized pair; every arm's observations remain raw evidence.
//! New Criterion cohorts never overwrite or claim fixed-count Python compatibility.

mod evidence;
mod format;
mod native;
mod remote;
mod workloads;

use anyhow::{Context, Result, ensure};
use criterion::{
    Criterion, SamplingMode, Throughput,
    measurement::{Measurement, ValueFormatter},
};
use evidence::{Config, Evidence};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

enum Experiment {
    Format(format::Format),
    Disk(Vec<Value>),
    Runtime {
        products: BTreeMap<String, native::Product>,
        image: Value,
        payload: Option<Vec<String>>,
    },
    Memory(Value),
    Remote(remote::Remote),
}
impl Experiment {
    fn prepare(e: &mut Evidence, config: &Config, entry: &str) -> Result<Self> {
        if entry == "image_format" {
            return Ok(Self::Format(format::Format::prepare(e, config)?));
        }
        e.prepare_inputs(config)?;
        if ["lazy_storage", "image_storage"].contains(&entry) {
            let images = if config.image == "combined" {
                e.images.clone()
            } else {
                vec![
                    e.images
                        .iter()
                        .find(|i| i["name"] == config.image)
                        .context("selected image absent")?
                        .clone(),
                ]
            };
            return Ok(Self::Disk(images));
        }
        if entry == "lazy_remote" {
            return Ok(Self::Remote(remote::Remote::prepare(
                e,
                config,
                &config.workload,
            )?));
        }
        ensure!(
            ["lazy_runtime", "native_runtime", "lazy_local_reads"].contains(&entry),
            "unknown storage entry"
        );
        let local = entry == "lazy_local_reads";
        let image = e
            .images
            .iter()
            .find(|i| {
                i["name"]
                    == if local {
                        "python-3.12-slim-bookworm"
                    } else {
                        "ubuntu-24.04"
                    }
            })
            .context("required image absent")?
            .clone();
        if !local
            && ["empty", "one", "four", "one_increment", "four_increment"]
                .iter()
                .any(|suffix| config.metric.ends_with(&format!(".{suffix}")))
        {
            return Ok(Self::Memory(image));
        }
        let payload = if local {
            ensure!(
                ["python", "mmap-scan"].contains(&config.workload.as_str()),
                "local workload must be python or mmap-scan"
            );
            let oracle = workloads::tree_oracle(&image)?;
            evidence::save(&e.root.join("mmap-oci-oracle.json"), &oracle)?;
            Some(workloads::payload(&config.workload, &oracle, true)?)
        } else {
            None
        };
        evidence::save(
            &e.root.join("workload.json"),
            &json!({"payload":payload,"image":image["pinned"]}),
        )?;
        let mut products = BTreeMap::new();
        let result = (|| -> Result<()> {
            for backend in ["docker", "pvisor"] {
                let directory = e.root.join(format!("prepared-{backend}"));
                let product = native::Product::new(e, directory, backend, &config.worker)?;
                // Insert immediately so even failed import is cleaned up.
                products.insert(backend.into(), product);
                let product = &products[backend];
                native::import(e, &image, backend, &product.prefix, &product.env)?;
                evidence::save(
                    &product.directory.join("prepared-resources.json"),
                    &product.resources()?,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for p in products.values_mut() {
                p.close(e);
            }
            return Err(error);
        }
        Ok(Self::Runtime {
            products,
            image,
            payload,
        })
    }
    fn operation(&mut self, e: &mut Evidence, config: &Config) -> Result<Value> {
        match self {
            Self::Format(f) => f.pair(e),
            Self::Disk(images) => native::disk(e, &config.worker, images, &config.image),
            Self::Runtime {
                products,
                image,
                payload,
            } => native::runtime_pair(e, products, image, payload.as_deref()),
            Self::Memory(image) => native::memory_pair(e, &config.worker, image),
            Self::Remote(r) => r.pair(e),
        }
    }
    fn close(&mut self, e: &mut Evidence) -> Result<()> {
        if let Self::Runtime { products, .. } = self {
            for product in products.values_mut() {
                product.close(e);
            }
        }
        if let Self::Remote(remote) = self {
            remote.close(e)?;
        }
        Ok(())
    }
}

/// Register only the explicitly selected storage entry.
///
/// Requires `PVISOR_BENCH_ENTRY=storage/<stem>` and a JSON configuration path in
/// `PVISOR_STORAGE_CONFIG`. No entry means no preparation, services or measurements.
/// A complete operation executes both randomized arms, but reports only the named
/// real metric to Criterion. Other metrics stay in the immutable per-arm evidence.
pub fn register(c: &mut Criterion) {
    let Ok(entry) = std::env::var("PVISOR_BENCH_ENTRY") else {
        return;
    };
    let Some(entry) = entry.strip_prefix("storage/") else {
        return;
    };
    let entry = entry.trim_end_matches(".json");
    let valid = [
        "lazy_local_reads",
        "lazy_storage",
        "lazy_runtime",
        "lazy_remote",
        "image_format",
        "image_storage",
        "native_runtime",
    ];
    if !valid.contains(&entry) {
        return;
    }
    let result = run(c, entry);
    if let Err(error) = result {
        panic!("storage experiment failed: {error:#}");
    }
}
fn run(c: &mut Criterion, entry: &str) -> Result<()> {
    let config_path = std::env::var("PVISOR_STORAGE_CONFIG")
        .context("explicit PVISOR_STORAGE_CONFIG JSON path required")?;
    let mut config: Config = serde_json::from_slice(&std::fs::read(config_path)?)?;
    if let Some(output) = std::env::var_os("PVISOR_BENCH_OUTPUT") {
        config.output = std::path::PathBuf::from(output).join("storage-evidence");
    }
    let unit = validate_metric(entry, &config)?;
    let mut e = Evidence::new(&config, entry)?;
    config.worker = e.root.join("storage-worker");
    e.manifest["metric"] = json!({"name":config.metric,"unit":unit,"scope":"sum of selected real arm observations divided by Criterion iteration count; paired arm not included in this metric"});
    e.manifest["benchmark_id"] = json!(match entry {
        "image_format" => "B-IMAGE-FORMAT-ENG",
        "image_storage" => "B-IMAGE-STORAGE-ENG",
        "native_runtime" => "B-NATIVE-DOCKER-ENG",
        _ => "B-LAZY-STARTUP",
    });
    e.manifest["role"] = json!(if ["image_format", "image_storage", "native_runtime"]
        .contains(&entry)
    {
        "engineering A/B"
    } else {
        "user-facing"
    });
    e.persist()?;
    let mut experiment = match Experiment::prepare(&mut e, &config, entry) {
        Ok(experiment) => experiment,
        Err(error) => {
            let result = Err(error);
            e.finish(&result)?;
            return result;
        }
    };
    e.manifest["status"] = json!("measuring");
    e.persist()?;
    let name = format!("storage/{entry}/{}/{}", config.workload, config.metric);
    let mut operations = 0u64;
    let mut invocations = 0u64;
    let measured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if unit == "ns" {
            let mut group = c.benchmark_group(format!("storage/{entry}"));
            group
                .sample_size(30)
                .sampling_mode(SamplingMode::Flat)
                .warm_up_time(Duration::from_secs(3));
            group.bench_function(&name,|b|b.iter_custom(|iterations|{
                invocations+=1;
                let invocation=invocations;
                let mut total=0u64;
                for _ in 0..iterations{
                    let first=e.number;
                    let observation=experiment.operation(&mut e,&config).expect("storage pair gates failed; retained evidence");
                    let value=evidence::metric(&observation,&config.metric).expect("metric absent");
                    ensure_integer(value).expect("invalid nanosecond metric");
                    total=total.checked_add(value as u64).expect("timing sum overflow");
                    operations+=1;
                    evidence::save(&e.root.join("criterion-operations").join(format!("{operations:09}.json")),&json!({"invocation":invocation,"requested_iterations":iterations,"first_evidence_sequence":first,"last_evidence_sequence":e.number,"metric":config.metric,"value":value,"unit":unit})).expect("retain Criterion operation mapping");
                }Duration::from_nanos(total)
            }));
            group.finish();
        } else {
            // Criterion's Measurement is a type parameter. A local instance is
            // required to keep byte/count values out of WallTime's nanoseconds.
            let mut measured = Criterion::default()
                .with_measurement(Units {
                    formatter: Formatter { unit },
                })
                .output_directory(&e.root.join("criterion"));
            if let Ok(path) = std::env::var("PVISOR_CRITERION_OUTPUT") {
                measured = measured.output_directory(Path::new(&path));
            }
            if let Ok(name) = std::env::var("PVISOR_CRITERION_SAVE_BASELINE") {
                measured = measured.save_baseline(name);
            }
            if let Ok(name) = std::env::var("PVISOR_CRITERION_BASELINE") {
                measured = measured.retain_baseline(name, true);
            }
            let mut group = measured.benchmark_group(format!("storage/{entry}"));
            group
                .sample_size(30)
                .sampling_mode(SamplingMode::Flat)
                .warm_up_time(Duration::from_secs(3));
            group.bench_function(&name,|b|b.iter_custom(|iterations|{
                invocations+=1;
                let invocation=invocations;
                let mut total=0.;
                for _ in 0..iterations{
                    let first=e.number;
                    let observation=experiment.operation(&mut e,&config).expect("storage pair gates failed; retained evidence");
                    let value=evidence::metric(&observation,&config.metric).expect("metric absent");
                    total+=value;operations+=1;
                    evidence::save(&e.root.join("criterion-operations").join(format!("{operations:09}.json")),&json!({"invocation":invocation,"requested_iterations":iterations,"first_evidence_sequence":first,"last_evidence_sequence":e.number,"metric":config.metric,"value":value,"unit":unit})).expect("retain Criterion operation mapping");
                }total
            }));
            group.finish();
            measured.final_summary();
        }
    }));
    let cleanup = experiment.close(&mut e);
    // Drop stops the remote worker before finalizing service-cleanup evidence.
    drop(experiment);
    let result = measured
        .map_err(|_| anyhow::anyhow!("Criterion operation panicked; entire cohort invalid"))
        .and(cleanup);
    e.manifest["operations"] = json!(operations);
    e.manifest["criterion_invocations"] = json!(invocations);
    let result = result.and_then(|()| {
        ensure!(
            operations > 0,
            "no Criterion operations executed; not a measured cohort"
        );
        Ok(())
    });
    e.finish(&result)?;
    result
}
fn ensure_integer(value: f64) -> Result<()> {
    ensure!(
        value >= 0. && value <= (1u64 << 53) as f64 && value.fract() == 0.,
        "integer metric outside exact f64 range"
    );
    Ok(())
}
fn validate_metric(entry: &str, config: &Config) -> Result<&'static str> {
    let components = config.metric.split('.').collect::<Vec<_>>();
    ensure!(
        components.len() >= 2,
        "metric must identify arm and observation"
    );
    let key = *components.last().unwrap();
    if entry == "image_format" {
        ensure!(
            ["baseline", "candidate"].contains(&components[0])
                && [
                    "open_ns",
                    "cold_stat_ns",
                    "warm_stat_ns",
                    "cold_read_ns",
                    "warm_read_ns",
                    "scan_ns"
                ]
                .contains(&key),
            "unsupported format metric"
        );
        return Ok("ns");
    }
    if ["image_storage", "lazy_storage"].contains(&entry) {
        ensure!(
            [
                "docker",
                "pvisor",
                "docker_empty",
                "docker_content",
                "docker_snapshots"
            ]
            .contains(&components[0])
                && ["apparent", "allocated", "inodes"].contains(&key),
            "unsupported disk metric"
        );
        return Ok(if key == "inodes" { "inodes" } else { "bytes" });
    }
    if entry == "lazy_remote" {
        ensure!(
            ["docker-cold", "docker-warm", "pvisor-cold", "pvisor-warm"].contains(&components[0]),
            "remote arm must include cold/warm"
        );
        ensure!(
            [
                "ready_ns",
                "exit_ns",
                "content_bytes",
                "metadata_bytes",
                "response_bytes",
                "requests"
            ]
            .contains(&key),
            "unsupported remote metric"
        );
        return Ok(if key.ends_with("_ns") {
            "ns"
        } else if key == "requests" {
            "requests"
        } else {
            "bytes"
        });
    }
    ensure!(
        ["docker", "pvisor"].contains(&components[0]),
        "unknown runtime arm"
    );
    ensure!(
        [
            "ready_ns",
            "exit_ns",
            "first_ns",
            "repeat_ns",
            "empty",
            "one",
            "four",
            "one_increment",
            "four_increment"
        ]
        .contains(&key),
        "unsupported runtime metric"
    );
    if ["first_ns", "repeat_ns"].contains(&key) {
        ensure!(
            entry == "lazy_local_reads" && config.workload == "mmap-scan",
            "internal scan clock requires mmap workload"
        );
    }
    if entry == "lazy_local_reads" {
        ensure!(
            key.ends_with("_ns"),
            "local reads does not perform memory rounds"
        );
    }
    Ok(if key.ends_with("_ns") { "ns" } else { "bytes" })
}
struct Units {
    formatter: Formatter,
}
impl Measurement for Units {
    type Intermediate = ();
    type Value = f64;
    fn start(&self) {}
    fn end(&self, _: ()) -> f64 {
        0.
    }
    fn add(&self, a: &f64, b: &f64) -> f64 {
        a + b
    }
    fn zero(&self) -> f64 {
        0.
    }
    fn to_f64(&self, value: &f64) -> f64 {
        *value
    }
    fn formatter(&self) -> &dyn ValueFormatter {
        &self.formatter
    }
}
struct Formatter {
    unit: &'static str,
}
impl ValueFormatter for Formatter {
    fn format_value(&self, value: f64) -> String {
        format!("{value:.2} {}", self.unit)
    }
    fn format_throughput(&self, _: &Throughput, value: f64) -> String {
        self.format_value(value)
    }
    fn scale_values(&self, _: f64, _: &mut [f64]) -> &'static str {
        self.unit
    }
    fn scale_throughputs(&self, _: f64, _: &Throughput, _: &mut [f64]) -> &'static str {
        self.unit
    }
    fn scale_for_machines(&self, _: &mut [f64]) -> &'static str {
        self.unit
    }
}

/// Dispatch dedicated worker operations; never runs a sampling loop.
pub fn worker_main() -> Result<()> {
    let argv = std::env::args().skip(1).collect::<Vec<_>>();
    match argv.first().map(String::as_str) {
        Some("inventory") => {
            ensure!(argv.len() == 2, "inventory ROOT");
            println!("{}", evidence::inventory(Path::new(&argv[1]))?);
            Ok(())
        }
        Some("registry") => {
            ensure!(argv.len() == 2, "registry CONFIG");
            remote::registry(Path::new(&argv[1]))
        }
        Some("remote-worker") => {
            ensure!(argv.len() == 2, "remote-worker BATCH");
            remote::worker(Path::new(&argv[1]))
        }
        Some("enter-group") => {
            use std::os::unix::process::CommandExt;
            ensure!(argv.len() >= 3, "enter-group OWNED_LEAF COMMAND...");
            let group = Path::new(&argv[1]);
            ensure!(
                group.starts_with("/sys/fs/cgroup/user.slice")
                    && !group
                        .components()
                        .any(|p| p == std::path::Component::ParentDir),
                "not delegated user leaf"
            );
            std::fs::write(group.join("cgroup.procs"), std::process::id().to_string())?;
            Err(std::process::Command::new(&argv[2])
                .args(&argv[3..])
                .exec()
                .into())
        }
        _ => anyhow::bail!(
            "storage worker operations: inventory, registry, remote-worker, enter-group; sampling is only through Criterion"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_time_formatter_does_not_encode_bytes_as_nanoseconds() {
        let units = Units {
            formatter: Formatter { unit: "bytes" },
        };
        assert_eq!(units.formatter().format_value(4096.), "4096.00 bytes");
        assert_eq!(units.add(&1024., &2048.), 3072.);
        assert_eq!(units.to_f64(&units.zero()), 0.);
    }
    #[test]
    fn selected_clocks_require_exact_nonnegative_integers() {
        assert!(ensure_integer(100.).is_ok());
        for n in [-1., 0.5, f64::NAN, f64::INFINITY] {
            assert!(ensure_integer(n).is_err());
        }
    }
}

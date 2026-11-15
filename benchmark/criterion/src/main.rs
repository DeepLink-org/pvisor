//! Criterion entry; experiments, not a Markdown benchmark language.
mod command;
mod filesystem;
mod harness;
mod memory;
mod network;
mod process;
mod replay;
#[cfg(target_os = "linux")]
mod startup;
#[cfg(all(target_os = "linux", feature = "storage-tls"))]
mod storage;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use criterion::{Criterion, SamplingMode};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(about = "Registered pVisor Criterion experiments; never publishes measurements")]
struct Args {
    /// Registry entry or documented alias. Defaults to process smoke.
    #[arg(default_value = "smoke")]
    entry: String,
    /// Prepared product executable (no build or download is performed).
    #[arg(long)]
    binary: Option<PathBuf>,
    /// Fresh raw evidence directory.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Shared Criterion result directory, including saved baselines.
    #[arg(long)]
    criterion_output: Option<PathBuf>,
    /// List registered entries and readiness without running commands.
    #[arg(long)]
    list: bool,
    /// Tool-independent, short Criterion boundary smoke, NOT product evidence.
    #[arg(long)]
    smoke: bool,
    /// Replay frozen process argv for diagnosis, outside all samples.
    #[arg(long)]
    diagnose: Option<PathBuf>,
    /// Criterion baseline options: --save-baseline NAME or --baseline NAME.
    #[arg(last = true, allow_hyphen_values = true)]
    criterion_args: Vec<String>,
}

fn resolve(entry: &str) -> Result<(&'static str, &'static str)> {
    let alias = match entry {
        "smoke" => "process/smoke.md",
        "nightly" => "process/nightly.md",
        "replay" => "replay/replay.md",
        "startup/timing" => "startup/timing.json",
        "filesystem/tools" => "filesystem/tools.md",
        other => other,
    };
    let mut matches = Vec::new();
    for line in include_str!("../../registry.csv").lines().skip(1) {
        let fields: Vec<_> = line.split(',').collect();
        ensure!(fields.len() == 5, "invalid registry row");
        let stem = fields[2]
            .strip_suffix(".json")
            .or_else(|| fields[2].strip_suffix(".md"))
            .unwrap_or(fields[2]);
        if fields[2] == alias
            || stem == alias
            || (!alias.contains('/') && stem.rsplit('/').next() == Some(alias))
        {
            matches.push(fields);
        }
    }
    ensure!(
        matches.len() == 1,
        "unknown or ambiguous benchmark entry {entry:?}; use --list"
    );
    let fields = &matches[0];
    ensure!(
        fields[3] != "requires-single-condition-command",
        "{} is registered but unprepared; no measurement performed",
        fields[2]
    );
    ensure!(
        fields[3] == "criterion-rust" && !fields[4].is_empty(),
        "registered entry lacks a Rust implementation"
    );
    Ok((fields[2], fields[2].split('/').next().unwrap()))
}

fn configure(args: &Args) -> Criterion {
    let mut c = Criterion::default();
    if let Some(path) = &args.criterion_output {
        c = c.output_directory(path);
        unsafe {
            std::env::set_var("PVISOR_CRITERION_OUTPUT", path);
        }
    }
    c.with_output_color(false)
}

fn run() -> Result<()> {
    let args = Args::parse();
    if args.list {
        print!("{}", include_str!("../../registry.csv"));
        return Ok(());
    }
    if let Some(batch) = args.diagnose {
        return process::diagnose(&batch);
    }
    // Criterion's own parser sees only its options, not this tool's arguments.
    // configure_from_args uses process args, so explicit stable builder options
    // are used here rather than handing two incompatible CLIs the same argv.
    let mut c = configure(&args);
    let mut flags = args.criterion_args.iter();
    while let Some(flag) = flags.next() {
        c = match flag.as_str() {
            "--save-baseline" => {
                let name = flags.next().context("baseline name required")?.clone();
                ensure!(
                    !name.is_empty() && !name.contains(['/', '\\']),
                    "invalid baseline name"
                );
                unsafe {
                    std::env::set_var("PVISOR_CRITERION_SAVE_BASELINE", &name);
                }
                c.save_baseline(name)
            }
            "--baseline" => {
                let name = flags.next().context("baseline name required")?.clone();
                ensure!(
                    !name.is_empty() && !name.contains(['/', '\\']),
                    "invalid baseline name"
                );
                unsafe {
                    std::env::set_var("PVISOR_CRITERION_BASELINE", &name);
                }
                c.retain_baseline(name, true)
            }
            _ => bail!(
                "unsupported Criterion option {flag:?}; supported: --save-baseline NAME, --baseline NAME"
            ),
        };
    }
    if args.smoke {
        c = c
            .sample_size(10)
            .warm_up_time(Duration::from_millis(10))
            .measurement_time(Duration::from_millis(20));
        let mut group = c.benchmark_group("harness");
        group.sampling_mode(SamplingMode::Flat);
        group.bench_function("self-reported-boundary", |b| {
            b.iter_custom(|iters| {
                command::measured_total(iters, || {
                    // Deliberately expensive excluded work. Criterion gets exactly
                    // the self-reported duration, never this fixture overhead.
                    std::thread::sleep(Duration::from_micros(50));
                    Ok(Duration::from_micros(10))
                })
                .expect("harness operation")
            })
        });
        group.finish();
        c.final_summary();
        harness::bytes();
        return Ok(());
    }
    let (entry, family) = resolve(&args.entry)?;
    let output = args.output.unwrap_or_else(|| {
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.data")
            .join(format!("criterion-{ticks}-{}", std::process::id()))
    });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Set only our process's experiment configuration before any threads start.
    unsafe {
        std::env::set_var("PVISOR_BENCH_ENTRY", entry);
        std::env::set_var("PVISOR_BENCH_FAMILY", family);
        std::env::set_var("PVISOR_BENCH_OUTPUT", &output);
        if let Some(binary) = args.binary {
            std::env::set_var("PVISOR_BENCH_BINARY", binary.canonicalize()?);
        }
    }
    match family {
        "filesystem" => filesystem::register(&mut c),
        "memory" => memory::register(&mut c),
        "network" => network::register(&mut c),
        "process" => process::register(&mut c, entry.contains("nightly"))?,
        "replay" => replay::register(&mut c),
        "startup" => {
            #[cfg(target_os = "linux")]
            startup::register(&mut c);
            #[cfg(not(target_os = "linux"))]
            bail!("prepared startup/image experiments require Linux");
        }
        "storage" => {
            #[cfg(all(target_os = "linux", feature = "storage-tls"))]
            storage::register(&mut c);
            #[cfg(not(all(target_os = "linux", feature = "storage-tls")))]
            bail!("storage experiments require Linux and the storage-tls feature");
        }
        _ => bail!("experiment family {family:?} is not yet integrated"),
    }
    c.final_summary();
    Ok(())
}

fn main() {
    #[cfg(target_os = "linux")]
    if let Some(code) = startup::worker_dispatch() {
        std::process::exit(code);
    }
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_unprepared_is_not_arbitrary_command() {
        assert!(
            resolve("network/network.json")
                .unwrap_err()
                .to_string()
                .contains("unprepared")
        );
        assert_eq!(resolve("smoke").unwrap(), ("process/smoke.md", "process"));
        assert!(resolve("typo").is_err());
    }
}

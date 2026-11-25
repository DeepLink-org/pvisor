//! CI gate over Criterion-produced estimates. No sampling or recomputation of
//! statistics; this is not a randomized A/B claim or a documentation publisher.
mod vm_matrix;
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, ValueEnum)]
enum Suite {
    Process,
    VmGuest,
    VmStartup,
}

#[derive(Parser)]
struct Args {
    /// Shared Criterion result root.
    root: PathBuf,
    #[arg(long, value_enum, default_value = "process")]
    suite: Suite,
    #[arg(long, default_value = "candidate")]
    candidate: String,
    #[arg(long, default_value = "main")]
    baseline: String,
    #[arg(long, default_value = "15")]
    threshold: f64,
    #[arg(long)]
    output: PathBuf,
}

fn median(path: &Path) -> Result<f64> {
    let value: Value = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    let point = value["median"]["point_estimate"]
        .as_f64()
        .context("Criterion median estimate missing")?;
    ensure!(point.is_finite() && point > 0.0, "invalid Criterion median");
    Ok(point)
}

fn run(args: Args) -> Result<()> {
    ensure!(
        args.threshold.is_finite() && args.threshold >= 0.0,
        "invalid threshold"
    );
    ensure!(
        !args.candidate.contains(['/', '\\']) && !args.baseline.contains(['/', '\\']),
        "baseline names must be single directory names"
    );
    ensure!(
        args.candidate != args.baseline,
        "cannot gate a cohort against itself"
    );
    fs::create_dir(&args.output).context("comparison output must be fresh")?;
    let (title, group, conditions): (_, _, Vec<_>) = match args.suite {
        Suite::Process => (
            "Process",
            "process_smoke",
            ["native", "host-run", "status", "review"]
                .into_iter()
                .map(|name| (name.to_owned(), false))
                .collect(),
        ),
        Suite::VmGuest => (
            "VM guest",
            vm_matrix::GROUP,
            vm_matrix::cells()
                .map(|(backend, _, name)| (name, backend == vm_matrix::REFERENCE))
                .collect(),
        ),
        Suite::VmStartup => (
            "VM startup: Create → guest command start",
            vm_matrix::STARTUP_GROUP,
            vm_matrix::BACKENDS
                .into_iter()
                .map(|b| {
                    (
                        format!("{b}-{}", vm_matrix::DIRECT_START),
                        b == vm_matrix::REFERENCE,
                    )
                })
                .chain([(vm_matrix::WARM_START.into(), false)])
                .collect(),
        ),
    };
    let mut report = format!(
        "# {title} regression gate\n\n\
Criterion 0.7.0 median estimates; sequential cohorts, not a causal A/B result.\n\n\
| Condition | Baseline ns/op | Candidate ns/op | Change | Gate |\n\
| --- | ---: | ---: | ---: | --- |\n",
    );
    let mut rows = Vec::new();
    let mut passed = true;
    for (condition, reference) in conditions {
        let root = args.root.join(group).join(&condition);
        let candidate = median(&root.join(&args.candidate).join("estimates.json"))?;
        let baseline = median(&root.join(&args.baseline).join("estimates.json"))?;
        let change = 100.0 * (candidate / baseline - 1.0);
        let pass = (!reference).then(|| candidate <= baseline + baseline * args.threshold / 100.0);
        passed &= pass.unwrap_or(true);
        report.push_str(&format!(
            "| {condition} | {baseline:.2} | {candidate:.2} | {change:+.2}% | {} |\n",
            match pass {
                None => "REFERENCE",
                Some(true) => "PASS",
                Some(false) => "REGRESSION",
            }
        ));
        rows.push(json!({"condition":condition, "baseline_ns_per_op":baseline,
            "candidate_ns_per_op":candidate, "change_pct":change, "passed":pass}));
    }
    fs::write(args.output.join("report.md"), report)?;
    fs::write(
        args.output.join("comparison.json"),
        serde_json::to_vec_pretty(&json!({
            "source":"Criterion 0.7.0 estimates.json median.point_estimate",
            "threshold_pct":args.threshold, "passed":passed, "rows":rows
        }))?,
    )?;
    ensure!(
        passed,
        "{title} regression exceeds {}% threshold",
        args.threshold
    );
    Ok(())
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_accepts_fifteen_percent_and_rejects_sixteen_percent() {
        let d = tempfile::tempdir().unwrap();
        for (index, candidate) in [115.0, 116.0].into_iter().enumerate() {
            for condition in ["native", "host-run", "status", "review"] {
                for (arm, value) in [
                    ("main", 100.0),
                    (
                        "new",
                        if condition == "status" {
                            candidate
                        } else {
                            100.0
                        },
                    ),
                ] {
                    let root = d.path().join("process_smoke").join(condition).join(arm);
                    fs::create_dir_all(&root).unwrap();
                    fs::write(
                        root.join("estimates.json"),
                        serde_json::to_vec(&json!({"median":{"point_estimate":value}})).unwrap(),
                    )
                    .unwrap();
                }
            }
            let output = d.path().join(format!("comparison-{index}"));
            let result = run(Args {
                suite: Suite::Process,
                root: d.path().into(),
                candidate: "new".into(),
                baseline: "main".into(),
                threshold: 15.0,
                output: output.clone(),
            });
            assert_eq!(result.is_ok(), index == 0);
            let report: Value =
                serde_json::from_slice(&fs::read(output.join("comparison.json")).unwrap()).unwrap();
            assert_eq!(report["threshold_pct"], 15.0);
            assert_eq!(report["passed"], index == 0);
        }
    }

    #[test]
    fn vm_gate_requires_all_cells_and_never_averages_away_a_tmpfs_regression() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join(vm_matrix::GROUP);
        for (backend, _, name) in vm_matrix::cells() {
            for arm in ["main", "new"] {
                let cell = root.join(&name).join(arm);
                fs::create_dir_all(&cell).unwrap();
                let ns = if backend == vm_matrix::REFERENCE && arm == "new" {
                    200
                } else {
                    100
                };
                fs::write(
                    cell.join("estimates.json"),
                    json!({"median":{"point_estimate":ns}}).to_string(),
                )
                .unwrap();
            }
        }
        let gate = |output: &str| {
            run(Args {
                suite: Suite::VmGuest,
                root: d.path().into(),
                candidate: "new".into(),
                baseline: "main".into(),
                threshold: 15.0,
                output: d.path().join(output),
            })
        };
        gate("pass").unwrap();
        let path = root.join("rust-native-tmpfs-metadata/new/estimates.json");
        fs::write(&path, r#"{"median":{"point_estimate":116}}"#).unwrap();
        assert!(gate("slow").is_err());
        assert!(
            fs::read_to_string(d.path().join("slow/report.md"))
                .unwrap()
                .contains("rust-native-tmpfs-metadata | 100.00 | 116.00 | +16.00% | REGRESSION")
        );
        fs::write(&path, r#"{"median":{"point_estimate":100}}"#).unwrap();
        let dirscan = root.join("rust-native-tmpfs-dirscan/new/estimates.json");
        fs::write(&dirscan, r#"{"median":{"point_estimate":116}}"#).unwrap();
        assert!(gate("slow-dirscan").is_err());
        fs::write(dirscan, r#"{"median":{"point_estimate":100}}"#).unwrap();
        fs::remove_file(root.join("firecracker-cpu-integer/main/estimates.json")).unwrap();
        assert!(
            gate("missing")
                .unwrap_err()
                .to_string()
                .contains("firecracker-cpu-integer")
        );
    }

    #[test]
    fn old_guest_baseline_cannot_pass_the_expanded_gate() {
        let d = tempfile::tempdir().unwrap();
        let legacy = [
            "cpu-integer",
            "memory-copy",
            "disk-write-fsync",
            "tmpfs-write",
            "tmpfs-read",
            "tmpfs-metadata",
        ];
        for (_, metric, name) in vm_matrix::cells() {
            for arm in ["main", "new"] {
                if arm == "main" && !legacy.contains(&metric) {
                    continue;
                }
                let cell = d.path().join(vm_matrix::GROUP).join(&name).join(arm);
                fs::create_dir_all(&cell).unwrap();
                fs::write(
                    cell.join("estimates.json"),
                    r#"{"median":{"point_estimate":100}}"#,
                )
                .unwrap();
            }
        }
        let error = run(Args {
            suite: Suite::VmGuest,
            root: d.path().into(),
            candidate: "new".into(),
            baseline: "main".into(),
            threshold: 15.0,
            output: d.path().join("report"),
        })
        .unwrap_err();
        assert!(error.to_string().contains("main/estimates.json"));
    }

    #[test]
    fn startup_requires_a_confirmed_hit_stream_without_reading_guest_estimates() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("vm_guest_command_start");
        for name in [
            "linux-direct-command-start",
            "rust-native-direct-command-start",
            "firecracker-direct-command-start",
            "linux-warm-command-start",
        ] {
            for arm in ["main", "candidate"] {
                let dir = root.join(name).join(arm);
                fs::create_dir_all(&dir).unwrap();
                let median = if arm == "candidate" && name == "firecracker-direct-command-start" {
                    200
                } else {
                    100
                };
                fs::write(
                    dir.join("estimates.json"),
                    json!({"median":{"point_estimate":median}}).to_string(),
                )
                .unwrap();
            }
        }
        let gate = |out: &str| {
            run(Args {
                suite: Suite::VmStartup,
                root: d.path().into(),
                baseline: "main".into(),
                candidate: "candidate".into(),
                threshold: 15.0,
                output: d.path().join(out),
            })
        };
        gate("pass").unwrap();
        let hit = root.join("linux-warm-command-start/candidate/estimates.json");
        fs::write(&hit, r#"{"median":{"point_estimate":116}}"#).unwrap();
        assert!(gate("slow").is_err());
        fs::remove_file(hit).unwrap();
        assert!(
            gate("missing")
                .unwrap_err()
                .to_string()
                .contains("linux-warm-command-start")
        );
    }

    #[test]
    fn invalid_estimates_are_errors_not_zero() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("estimates.json");
        for data in [
            r#"{}"#,
            r#"{"median":{"point_estimate":0}}"#,
            r#"{"median":{"point_estimate":-1}}"#,
        ] {
            fs::write(&path, data).unwrap();
            assert!(median(&path).is_err());
        }
        fs::write(&path, r#"{"median":{"point_estimate":15.5}}"#).unwrap();
        assert_eq!(median(&path).unwrap(), 15.5);
    }
}

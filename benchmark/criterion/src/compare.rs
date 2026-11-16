//! CI gate over Criterion-produced estimates. No sampling or recomputation of
//! statistics; this is not a randomized A/B claim or a documentation publisher.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    /// Shared Criterion result root.
    root: PathBuf,
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

fn run() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.threshold.is_finite() && args.threshold >= 0.0,
        "invalid threshold"
    );
    ensure!(
        !args.candidate.contains(['/', '\\']) && !args.baseline.contains(['/', '\\']),
        "baseline names must be single directory names"
    );
    fs::create_dir(&args.output).context("comparison output must be fresh")?;
    let mut report = String::from(
        "# Process regression gate\n\n\
Criterion 0.7.0 median estimates; sequential cohorts, not a causal A/B result.\n\n\
| Condition | Baseline ns/op | Candidate ns/op | Change | Gate |\n\
| --- | ---: | ---: | ---: | --- |\n",
    );
    let mut rows = Vec::new();
    let mut passed = true;
    for condition in ["native", "host-run", "status", "review"] {
        let root = args.root.join("process_smoke").join(condition);
        let candidate = median(&root.join(&args.candidate).join("estimates.json"))?;
        let baseline = median(&root.join(&args.baseline).join("estimates.json"))?;
        let change = 100.0 * (candidate / baseline - 1.0);
        let pass = change <= args.threshold;
        passed &= pass;
        report.push_str(&format!(
            "| {condition} | {baseline:.2} | {candidate:.2} | {change:+.2}% | {} |\n",
            if pass { "PASS" } else { "REGRESSION" }
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
        "process regression exceeds {}% threshold",
        args.threshold
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

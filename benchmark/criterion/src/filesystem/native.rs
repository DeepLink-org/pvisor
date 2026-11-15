use super::support::*;
use anyhow::Result;
use criterion::Criterion;
use serde_json::json;
use std::{fs, process::Command, time::Duration};

pub fn register(c: &mut Criterion) {
    let result = (|| -> Result<()> {
        let output = output()?;
        let source = required("PVISOR_BENCH_FS_WORKER")?;
        let worker = output.join("filesystem-worker");
        fs::copy(&source, &worker)?;
        let digest = sha(&worker)?;
        save(
            output.join("manifest.json"),
            &json!({"benchmark":"B-FS-TOOLS","engine":"criterion-0.7.0",
            "metric":"complete native Rust worker command","worker_source":source,"worker_sha256":digest,
            "adapter_source":include_str!("native.rs"),"subject_source":include_str!("worker.rs"),
            "minimum_statistical_samples":30,"raw_observations_include_calibration":true}),
        )?;
        let fixture = output.join("fixture");
        checked(Command::new(&worker).arg("prepare").arg(&fixture))?;
        let mut group = c.benchmark_group("filesystem/tools");
        group.sample_size(30);
        for mode in ["metadata", "read"] {
            let mut index = 0u64;
            group.bench_function(mode, |b| {
                b.iter_custom(|iterations| {
                    let mut measured = Duration::ZERO;
                    for _ in 0..iterations {
                        // Criterion invokes this for calibration and warmup as well as samples.
                        checked(Command::new(&worker).arg("validate").arg(&fixture)).unwrap();
                        let folder = output.join(format!("{mode}-{index:08}"));
                        index += 1;
                        let (elapsed, text) = run(
                            Command::new(&worker).arg(mode).arg(&fixture),
                            &folder,
                            Duration::from_secs(30),
                            &[],
                        )
                        .unwrap();
                        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
                        assert_eq!(row, json!({"workload":mode,"correctness":"passed"}));
                        checked(Command::new(&worker).arg("validate").arg(&fixture)).unwrap();
                        measured += elapsed;
                    }
                    measured
                })
            });
        }
        group.finish();
        anyhow::ensure!(sha(&worker)? == digest, "frozen native worker changed");
        fs::write(
            output.join("metric-scope.txt"),
            "Rust worker creation through successful exit and pipe/log collection; not historical Python command time.\n",
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        panic!("filesystem/tools: {error:#}");
    }
}

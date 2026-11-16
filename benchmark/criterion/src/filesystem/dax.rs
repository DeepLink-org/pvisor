//! Immutable-share mount-toggle experiment; the parent owns every VM stage.
use super::support::*;
use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    process::Command,
    time::Duration,
};

const GATE: [&str; 9] = [
    "cargo",
    "rustc",
    "pytest",
    "nextest",
    "hyperfine",
    "ninja",
    "make",
    "cargo-nextest",
    "cargo-zigbuild",
];
const METRICS: [&str; 10] = [
    "metadata_first",
    "metadata_repeat",
    "small_first",
    "small_repeat",
    "read_first",
    "read_repeat",
    "mmap_first",
    "mmap_repeat",
    "parallel_first",
    "parallel_repeat",
];
fn pattern(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed;
    let mut output = Vec::with_capacity(len);
    for _ in (0..len).step_by(4) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        output.extend_from_slice(&x.to_le_bytes());
    }
    output.truncate(len);
    output
}
fn fixture(root: &Path, guest: &Path) -> Result<Value> {
    fs::create_dir(root)?;
    for (name, seed) in [("sequential", 101), ("mapped", 202), ("parallel", 303)] {
        let bytes = pattern(seed, 1 << 20);
        let mut file = File::create(root.join(name))?;
        for _ in 0..64 {
            file.write_all(&bytes)?;
        }
    }
    fs::create_dir(root.join("small"))?;
    for i in 0..128 {
        fs::write(
            root.join(format!("small/f{i:03}")),
            pattern(1000 + i, 64 << 10),
        )?;
    }
    for i in 0..2048 {
        let dir = root.join(format!("meta/d{:02}", i / 64));
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(format!("f{i:04}")),
            (0..256).map(|i| i as u8).collect::<Vec<_>>(),
        )?;
    }
    fs::copy(guest, root.join("workload"))?;
    tree(root)
}
fn tree(root: &Path) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    fn visit(root: &Path, path: &Path, items: &mut serde_json::Map<String, Value>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            let mut row = json!({"mode":meta.mode(),"size":if meta.is_file(){Some(meta.len())}else{None},
                "mtime":meta.mtime(),"mtime_nsec":meta.mtime_nsec(),"ctime":meta.ctime(),"ctime_nsec":meta.ctime_nsec()});
            if meta.is_symlink() {
                row["link"] = json!(fs::read_link(&path)?);
            } else if meta.is_file() {
                row["sha256"] = json!(sha(&path)?);
            }
            items.insert(path.strip_prefix(root)?.to_string_lossy().into_owned(), row);
            if meta.is_dir() {
                visit(root, &path, items)?;
            }
        }
        Ok(())
    }
    let mut items = serde_json::Map::new();
    visit(root, root, &mut items)?;
    Ok(Value::Object(items))
}
fn validate(value: &Value, mode: &str) -> Result<()> {
    ensure!(
        value["mode"] == mode
            && value["dax"] == (mode == "on")
            && value["verified_bytes"] == 3 * (64 << 20) + 128 * (64 << 10)
            && value["metadata_files"] == 2048,
        "DAX correctness/attribute check"
    );
    let metrics = value["metrics_ns"].as_object().context("metrics")?;
    ensure!(
        metrics.len() == 10
            && METRICS
                .iter()
                .all(|k| metrics.get(*k).and_then(Value::as_u64).unwrap_or(0) > 0),
        "DAX metric inventory"
    );
    let totals = value["phase_totals_ns"]
        .as_object()
        .context("Rust guest phase totals")?;
    ensure!(
        value["subject_language"] == "rust"
            && totals.len() == 10
            && METRICS
                .iter()
                .all(|k| totals.get(*k).and_then(Value::as_u64).unwrap_or(0) > 0),
        "Rust guest full phase clocks"
    );
    for key in METRICS {
        let repetitions = match key {
            "read_repeat" | "parallel_repeat" => 4,
            "mmap_repeat" => 16,
            _ => 1,
        };
        let mean = metrics[key].as_u64().unwrap();
        let total = totals[key].as_u64().unwrap();
        ensure!(
            total / repetitions == mean,
            "DAX repeat mean/total clock mismatch"
        );
    }
    Ok(())
}
pub fn register(c: &mut Criterion) {
    let result = (|| -> Result<()> {
        let selected = std::env::var("PVISOR_BENCH_METRIC").unwrap_or_else(|_| "read_first".into());
        ensure!(METRICS.contains(&selected.as_str()), "unknown DAX phase");
        let output = output()?;
        let driver = required("PVISOR_BENCH_DAX_DRIVER")?;
        let guest = required("PVISOR_BENCH_DAX_GUEST")?;
        let rootfs = required("PVISOR_BENCH_ROOTFS")?;
        let firmware = required("PVISOR_BENCH_FIRMWARE")?;
        let frozen = output.join("bin");
        fs::create_dir(&frozen)?;
        fs::copy(driver, frozen.join("subject"))?;
        fs::copy(firmware, frozen.join("libkrunfw.so"))?;
        let inputs = fixture(&output.join("immutable"), &guest)?;
        let before = budget(None, true)?;
        save(output.join("budget-before.json"), &before)?;
        let mut hashes = serde_json::Map::new();
        for name in ["subject", "libkrunfw.so"] {
            hashes.insert(format!("bin/{name}"), json!(sha(&frozen.join(name))?));
        }
        let hashes = Value::Object(hashes);
        save(
            output.join("protocol.json"),
            &json!({"benchmark":"B-FS-ENG","role":"engineering A/B",
            "metric_scope":"selected arm real complete guest phase interval",
            "selected_phase":selected,
            "warmups_per_arm":3,"minimum_pairs":30,"seed":20261011,"rng":"xorshift64",
            "input_before":inputs,"artifacts":hashes,"driver_source":include_str!("../../../filesystem/dax_driver.rs"),
            "guest_source":include_str!("dax_guest.rs"),"rootfs":rootfs}),
        )?;
        let mut index = 0;
        let mut records = vec![];
        let mut rng = Rng(20261011);
        let mut reversed_block = false;
        let mut trial = |phase: &str, index: u64, mode: &str| -> Result<Duration> {
            let folder = output.join(format!("{phase}-{index:08}-{mode}"));
            fs::create_dir(&folder)?;
            for name in ["stage/upper", "stage/work"] {
                fs::create_dir_all(folder.join(name))?;
            }
            let mut command = Command::new(frozen.join("subject"));
            command
                .arg(&rootfs)
                .arg(frozen.join("libkrunfw.so"))
                .arg(output.join("immutable"))
                .arg(folder.join("stage"))
                .arg(mode);
            clean(&mut command);
            let (wall, text) = run(&mut command, &folder, Duration::from_secs(60), &GATE)?;
            let value = unique(&text, "DAX_BENCH_RESULT ")?;
            validate(&value, mode)?;
            save(
                folder.join("result.json"),
                &json!({"phase":phase,"index":index,"mode":mode,"observed":value,"wall_ns":wall.as_nanos()}),
            )?;
            records.push(json!({"phase":phase,"index":index,"mode":mode,"observed":value}));
            // VM shutdown can exit its process; cleanup must belong to this observer.
            fs::remove_dir_all(folder.join("stage"))?;
            Ok(Duration::from_nanos(
                value["phase_totals_ns"][&selected]
                    .as_u64()
                    .context("selected DAX phase")?,
            ))
        };
        for i in 0..3 {
            for mode in ["off", "on"] {
                trial("warmup", i, mode)?;
            }
        }
        let mut group = c.benchmark_group("filesystem/dax_ab");
        group.sample_size(30).sampling_mode(SamplingMode::Flat);
        for arm in ["off", "on"] {
            group.bench_function(format!("{selected}/{arm}/ns-per-phase"), |b| {
                b.iter_custom(|iterations| {
                    let mut measured = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut order = ["off", "on"];
                        // Balanced blocks retain one order of each kind, randomized by block.
                        if index % 2 == 0 {
                            reversed_block = rng.next() & 1 == 1;
                        }
                        if reversed_block ^ (index % 2 == 1) {
                            order.reverse();
                        }
                        save(
                            output.join(format!("pair-{index:08}-order.json")),
                            &json!(order),
                        )
                        .unwrap();
                        for mode in order {
                            let elapsed = trial("criterion", index, mode).expect("checked DAX VM");
                            if mode == arm {
                                measured += elapsed;
                            }
                        }
                        index += 1;
                    }
                    measured
                })
            });
        }
        group.finish();
        drop(trial);
        ensure!(
            index >= 30 && records.len() == index as usize * 2 + 6,
            "incomplete DAX cohort"
        );
        verify_hashes(&output, &hashes)?;
        ensure!(
            tree(&output.join("immutable"))? == inputs,
            "immutable share changed"
        );
        let after = budget(None, true)?;
        unchanged_events(&before, &after)?;
        save(output.join("budget-after.json"), &after)?;
        save(output.join("records.json"), &json!(records))?;
        save(
            output.join("validation.json"),
            &json!({"valid":true,"selected_phase":selected,"checked_pairs":index,
            "criterion_owns_statistics":true,"raw_observations":"include calibration/warmup; never labelled formal samples",
            "paired_interpretation":"full randomized pairs retained; Criterion arm intervals are not paired contrast intervals"}),
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        panic!("filesystem/dax_ab: {error:#}");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern_matches_original_xorshift() {
        assert_eq!(pattern(101, 4), [3, 172, 152, 1]);
    }
    #[test]
    fn incomplete_dax_result_rejected() {
        assert!(validate(&json!({}), "off").is_err());
    }
}

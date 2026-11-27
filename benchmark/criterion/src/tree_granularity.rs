//! B-VM-GUEST-ENG: does batching at the producer remove the benefit of guest tmpfs staging?
//! Six native VMs per randomized complete block; compare the two verified publications at each size.
use crate::{
    command::measured_total,
    filesystem::support::{Rng, checked, clean, output, required, run, save, sha},
};
use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command, time::Duration};

const SEED: u64 = 21316;

fn complete_block(
    dir: &Path,
    driver: &Path,
    vm: &Path,
    probe: &Path,
    rng: &mut Rng,
) -> Result<Duration> {
    fs::create_dir(dir)?;
    let mut sizes = [1, 16, 256];
    rng.shuffle(&mut sizes);
    let mut total = Duration::ZERO;
    let mut rows = vec![];
    for kib in sizes {
        let mut arms = ["shared", "local"];
        rng.shuffle(&mut arms);
        for arm in arms {
            let logs = dir.join(format!("{kib}-{arm}"));
            let mut cmd = Command::new(driver);
            clean(&mut cmd);
            cmd.arg(vm)
                .arg(probe)
                .arg(arm)
                .arg(logs.join("trial"))
                .arg(kib.to_string());
            let (wall, stdout) = run(&mut cmd, &logs, Duration::from_secs(30), &[])
                .with_context(|| format!("see {}", logs.display()))?;
            let row: Value = serde_json::from_str(&stdout)?;
            ensure!(row["arm"] == arm && row["write_kib"] == kib, "wrong cell");
            let ns = row["end_to_end_ns"]
                .as_u64()
                .context("missing end-to-end time")?;
            ensure!(
                row["guest_publish_ns"]
                    .as_u64()
                    .is_some_and(|g| 0 < g && g <= ns)
                    && u128::from(ns) <= wall.as_nanos(),
                "invalid publication duration"
            );
            total += Duration::from_nanos(ns);
            rows.push(row);
        }
    }
    save(dir.join("block.json"), &json!(rows))?;
    Ok(total)
}

pub fn register(c: &mut Criterion) -> Result<()> {
    let batch = output()?;
    let vm = required("PVISOR_BENCH_BINARY")?;
    let probe = required("PVISOR_BENCH_GUEST")?;
    let driver = required("PVISOR_BENCH_TREE_ONCE")?;
    let backing = checked(Command::new("stat").args(["-f", "-c", "%T"]).arg(&batch))?;
    ensure!(
        matches!(backing.trim(), "btrfs" | "ext2/ext3" | "xfs"),
        "need a disk-backed batch"
    );
    let status = fs::read_to_string("/proc/self/status")?;
    save(
        batch.join("manifest.json"),
        &json!({
            "benchmark":"B-VM-GUEST-ENG", "backend":"rust-native", "vcpus":1, "memory_mib":256,
            "seed":SEED, "backing_fs":backing.trim(), "host":checked(Command::new("uname").arg("-a"))?.trim(),
            "affinity":status.lines().find(|s| s.starts_with("Cpus_allowed_list:")),
            "vm_sha256":sha(&vm)?, "guest_sha256":sha(&probe)?, "driver_sha256":sha(&driver)?,
            "harness_sha256":sha(&std::env::current_exe()?)?,
        }),
    )?;
    fs::write(
        batch.join("staged_tree.c"),
        include_str!("../../vm/staged_tree.c"),
    )?;
    fs::write(
        batch.join("packed_tree_once.rs"),
        include_str!("../../vm/packed_tree_once.rs"),
    )?;
    fs::write(
        batch.join("tree_granularity.rs"),
        include_str!("tree_granularity.rs"),
    )?;
    let mut group = c.benchmark_group("native-tree");
    group
        .sample_size(15)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(2));
    let mut rng = Rng(SEED);
    let mut block = 0u64;
    group.bench_function("producer-write-size", |b| {
        b.iter_custom(|iters| {
            measured_total(iters, || {
                block += 1;
                let dir = batch.join(format!("block-{block:04}"));
                complete_block(&dir, &driver, &vm, &probe, &mut rng)
            })
            .expect("six verified publications per block")
        })
    });
    group.finish();
    save(batch.join("complete.json"), &json!({"blocks":block}))
}

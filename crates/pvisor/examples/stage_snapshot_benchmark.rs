//! Filesystem/store microbenchmark. No VM/RAM capture latency claims.
//! cargo run --release -p pvisor --example stage_snapshot_benchmark -- OUTPUT.json
use anyhow::Context;
use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
use serde_json::json;
use std::{fs, io::Write, path::Path, time::Instant};

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "benchmark".into(),
        build: "benchmark".into(),
        firmware: "benchmark".into(),
        profile: "stage-store-benchmark".into(),
    }
}
fn run() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .context("output JSON path required")?;
    let mut cases = Vec::new();
    for megabytes in [1, 32, 128] {
        let temp = tempfile::tempdir()?;
        let base_root = temp.path().join("input");
        fs::create_dir(&base_root)?;
        let mut large = fs::File::create(base_root.join("unchanged"))?;
        let chunk = vec![37; 1024 * 1024];
        for _ in 0..megabytes {
            large.write_all(&chunk)?;
        }
        large.sync_all()?;
        // Grow file count too; these objects are never used by the fixed stage.
        fs::create_dir(base_root.join("files"))?;
        for n in 0..megabytes * 16 {
            fs::write(
                base_root.join("files").join(n.to_string()),
                b"unchanged input",
            )?;
        }
        let store_root = temp.path().join("store");
        let store = SnapshotStore::new(&store_root)?;
        let imported_at = Instant::now();
        let base = store.import_base(&base_root)?;
        let import_ms = imported_at.elapsed().as_secs_f64() * 1000.0;
        let stage = temp.path().join("stage");
        for part in ["upper", "work", "preimages"] {
            fs::create_dir_all(stage.join(part))?;
        }
        fs::write(stage.join("upper/change"), vec![7; 65536])?;
        fs::write(stage.join("work/journal"), b"stage state")?;
        fs::write(stage.join("preimages/observed"), b"fixed preimage")?;
        let mut samples = Vec::new();
        // Alternate order. Two warmups plus eight measured pairs, fixed RAM.
        for iteration in 0..10 {
            for stage_only in if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let pending = store.begin()?;
                pending.create_ram()?.write_all(&vec![1; 1024 * 1024])?;
                let saved_at = Instant::now();
                let id = if stage_only {
                    pending.publish_stage(
                        &stage,
                        std::slice::from_ref(&base),
                        b"fixed-machine",
                        compatibility(),
                        false,
                    )?
                } else {
                    pending.publish(&base_root, b"fixed-machine", compatibility())?
                };
                let save_ms = saved_at.elapsed().as_secs_f64() * 1000.0;
                let restore_at = Instant::now();
                let snapshot = store.open_for_restore(&id, &compatibility())?;
                let destination = temp
                    .path()
                    .join(format!("restore-{iteration}-{stage_only}"));
                if stage_only {
                    snapshot.materialize_stage(&destination)?;
                } else {
                    snapshot.materialize(&destination)?;
                }
                let restore_ms = restore_at.elapsed().as_secs_f64() * 1000.0;
                let logical_bytes: u64 = snapshot
                    .manifest()
                    .filesystem
                    .entries
                    .iter()
                    .map(|entry| {
                        if let pvisor::environment_snapshot::TreeObject::File { bytes, .. } =
                            entry.object
                        {
                            bytes
                        } else {
                            0
                        }
                    })
                    .sum();
                if stage_only {
                    anyhow::ensure!(
                        !destination.join("unchanged").exists(),
                        "base copied into stage"
                    );
                    anyhow::ensure!(
                        fs::read(destination.join("upper/change"))? == vec![7; 65536],
                        "stage mismatch"
                    );
                }
                if iteration >= 2 {
                    samples.push(json!({"iteration":iteration - 2,"stage_only":stage_only,"save_ms":save_ms,"restore_ms":restore_ms,"logical_payload_bytes":logical_bytes}));
                }
                drop(snapshot);
                store.delete(&id)?;
                fs::remove_dir_all(destination)?;
            }
        }
        cases.push(json!({"base_megabytes":megabytes,"base_small_files":megabytes * 16,"import_ms":import_ms,"samples":samples}));
    }
    let record = json!({"scope":"store publication and materialization; fixed 1 MiB already-captured RAM; stage has 64 KiB change plus fixed metadata; full-tree baseline preserves the unchanged input tree; no VM, freeze, guest or first-page latency measured", "warmups_per_variant":2,"measured_samples_per_variant":8,"host":std::env::consts::OS,"arch":std::env::consts::ARCH,"cases":cases});
    if let Some(parent) = Path::new(&output).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}
fn main() -> anyhow::Result<()> {
    run()
}

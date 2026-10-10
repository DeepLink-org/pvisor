//! Shared worker for B-FS-ENG read dispatch and native journal-lock A/B.
use anyhow::{ensure, Result};
use pvisor_overlay_core::{fingerprint_at, load_preimages, OverlayCore};
use pvisor_overlayfs::api::{
    OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySessionControl,
};
use std::{
    fs,
    io::{self, BufRead, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn journal(lower: PathBuf, stage: PathBuf, compact: bool) -> Result<()> {
    let directory = stage.join("preimages");
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![lower.clone()],
        stage.join("upper"),
        Some(stage.join("work")),
        vec![],
        Some(directory.clone()),
    )?;
    let core = Arc::new(if compact {
        core.with_compact_preimages()?
    } else {
        core
    });
    let start = Instant::now();
    for index in 0..64 {
        core.observe_read(Path::new(&format!("idle/f{index:02}")))?;
    }
    let idle_ms = start.elapsed().as_secs_f64() * 1000.0;
    let source = lower.join("slow");
    let before = fs::metadata(&source)?;
    let capture = core.clone();
    let worker = thread::spawn(move || -> Result<f64> {
        let start = Instant::now();
        capture.observe_read(Path::new("slow"))?;
        Ok(start.elapsed().as_secs_f64() * 1000.0)
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now = fs::metadata(&source)?;
        if (now.atime(), now.atime_nsec()) != (before.atime(), before.atime_nsec()) {
            break;
        }
        ensure!(
            !worker.is_finished() && Instant::now() < deadline,
            "missed fingerprint read window"
        );
        thread::sleep(Duration::from_micros(100));
    }
    ensure!(!worker.is_finished(), "fingerprint finished before probe");
    let start = Instant::now();
    core.observe_read(Path::new("probe/f00"))?;
    let first_ms = start.elapsed().as_secs_f64() * 1000.0;
    let first_during_hash = !worker.is_finished();
    for index in 1..64 {
        core.observe_read(Path::new(&format!("probe/f{index:02}")))?;
    }
    let batch_ms = start.elapsed().as_secs_f64() * 1000.0;
    let batch_during_hash = !worker.is_finished();
    let hash_ms = worker
        .join()
        .map_err(|_| anyhow::anyhow!("hash worker panicked"))??;
    core.sync_preimages()?;
    let entries = load_preimages(&directory)?;
    ensure!(entries.len() == 129, "missing journal entries");
    for entry in &entries {
        ensure!(
            entry.state == fingerprint_at(&lower, &entry.relative_path())?,
            "fingerprint mismatch"
        );
    }
    println!(
        r#"{{"idle_journal_ms":{idle_ms},"first_journal_ms":{first_ms},"journal_batch_ms":{batch_ms},"hash_ms":{hash_ms},"first_during_hash":{first_during_hash},"batch_during_hash":{batch_during_hash},"observations":129}}"#
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 4, "expected lower, stage, mount, condition");
    let lower = PathBuf::from(&args[0]);
    let stage = PathBuf::from(&args[1]);
    if args[3].starts_with("journal-") {
        ensure!(
            matches!(args[3].as_str(), "journal-legacy" | "journal-compact"),
            "unknown condition"
        );
        return journal(lower, stage, args[3] == "journal-compact");
    }
    let mut config = OverlayMountConfig::new(
        vec![lower],
        stage.join("upper"),
        Some(stage.join("work")),
        PathBuf::from(&args[2]),
    );
    config.backend = Some("fskit".into());
    match args[3].as_str() {
        "legacy" | "compact" => {
            config.preimage_dir = Some(stage.join("preimages"));
            config.compact_preimages = args[3] == "compact";
        }
        "nojournal" => {}
        _ => anyhow::bail!("unknown condition"),
    }
    let session = OverlayFs::mount(config)?;
    println!("ready");
    io::stdout().flush()?;
    ensure!(
        io::stdin().lock().lines().next().transpose()?.as_deref() == Some("stop"),
        "missing stop"
    );
    session.unmount()?;
    if args[3] != "nojournal" {
        let entries = load_preimages(&stage.join("preimages"))?;
        for name in ["slow", "held-probe", "probe/f00", "one", "two"] {
            ensure!(
                entries.iter().any(|entry| entry.path == name.as_bytes()),
                "missing preimage {name}"
            );
        }
    }
    println!("stopped");
    Ok(())
}

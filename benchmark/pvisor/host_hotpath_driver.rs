//! B-FS-ENG: identical mount owner for the two frozen host hot-path variants.
use anyhow::{Result, ensure};
use pvisor_overlay_core::LayerMutability;
use pvisor_overlayfs::api::{
    OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySessionControl,
};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 4, "expected lower, stage, mount, condition");
    // No worker threads exist yet; diagnostics never share the timing cohort.
    if args[3].ends_with("-profile") {
        unsafe {
            std::env::set_var("PVISOR_FS_PROFILE", "1");
        }
    }
    let stage = PathBuf::from(&args[1]);
    let condition = args[3].trim_end_matches("-profile");
    let mut config = OverlayMountConfig::new(
        vec![PathBuf::from(&args[0])],
        stage.join("upper"),
        Some(stage.join("work")),
        PathBuf::from(&args[2]),
    );
    config.backend = Some("fskit".into());
    match condition {
        "immutable-nojournal" => config.lower_mutability = vec![LayerMutability::Immutable],
        "mutable-compact" => {
            config.lower_mutability = vec![LayerMutability::Mutable];
            config.preimage_dir = Some(stage.join("preimages"));
            config.compact_preimages = true;
        }
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
    if condition == "mutable-compact" {
        let observations = pvisor_overlay_core::load_preimages(&stage.join("preimages"))?;
        for kind in ["append", "truncate"] {
            for index in 0..128 {
                let path = format!("{kind}/f{index:04}");
                ensure!(
                    observations.iter().any(|o| o.path == path.as_bytes()),
                    "missing {path} preimage"
                );
            }
        }
        std::fs::write(
            stage.join("observation-count.json"),
            observations.len().to_string(),
        )?;
    }
    println!("stopped");
    Ok(())
}

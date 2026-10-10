//! B-FS-ENG host copy-up A/B mount owner; identical in both source variants.
use anyhow::{ensure, Result};
use pvisor_overlayfs::api::{
    OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySessionControl,
};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4,
        "expected lower, stage, mount, journal condition"
    );
    let stage = PathBuf::from(&args[1]);
    let mut config = OverlayMountConfig::new(
        vec![PathBuf::from(&args[0])],
        stage.join("upper"),
        Some(stage.join("work")),
        PathBuf::from(&args[2]),
    );
    config.backend = Some("fskit".into());
    match args[3].as_str() {
        "compact" => {
            config.preimage_dir = Some(stage.join("preimages"));
            config.compact_preimages = true;
        }
        "nojournal" => {}
        _ => anyhow::bail!("unknown journal condition"),
    }
    let session = OverlayFs::mount(config)?;
    println!("ready");
    io::stdout().flush()?;
    ensure!(
        io::stdin().lock().lines().next().transpose()?.as_deref() == Some("stop"),
        "missing stop"
    );
    session.unmount()?;
    if args[3] == "compact" {
        let observations = pvisor_overlay_core::load_preimages(&stage.join("preimages"))?;
        ensure!(
            observations.iter().any(|entry| entry.path == b"slow"),
            "missing source preimage"
        );
    }
    println!("stopped");
    Ok(())
}

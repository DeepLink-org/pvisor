//! Benchmark-only owner for the runtime's existing public host FUSE image API.
//! Does not implement a filesystem or modify the product execution path.
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Instant;

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 || args[5] != "--" {
        return Err(
            "usage: lazy-stage-mount SERVER HANDLE STORE RECEIPT -- COMMAND ARGS...".into(),
        );
    }
    let start = Instant::now();
    let image = if args[1] == "--plain" {
        None
    } else {
        Some(pvisor::cache::open_image_handle_for_host(
            pvisor::cache::CacheConfig {
                backend: pvisor::cache::CacheBackend::Server,
                location: args[1].clone(),
                read_only: false,
                image_store: Some(PathBuf::from(&args[3])),
            },
            &args[2],
        )?)
    };
    let mounted = Instant::now();
    let root = image
        .as_ref()
        .map(|v| v.rootfs().to_owned())
        .unwrap_or_else(|| PathBuf::from(&args[2]));
    let receipt = format!(
        "rootfs={}\ndigest={}\nmount_ns={}\n",
        root.display(),
        image
            .as_ref()
            .map(|v| v.manifest_digest())
            .unwrap_or("overlaybd-view"),
        mounted.duration_since(start).as_nanos()
    );
    std::fs::write(&args[4], receipt)?;
    std::fs::write(
        format!("{}.mountinfo", args[4]),
        std::fs::read("/proc/self/mountinfo")?,
    )?;
    io::stderr().flush()?;
    let status = Command::new(&args[6])
        .args(&args[7..])
        .current_dir(&root)
        .status()?;
    std::fs::write(
        format!("{}.stage", args[4]),
        format!("completion_ns={}\n", mounted.elapsed().as_nanos()),
    )?;
    let is_lazy = image.is_some();
    drop(image);
    std::fs::write(
        format!("{}.unmounted", args[4]),
        format!("mountpoint_removed={}\n", !is_lazy || !root.exists()),
    )?;
    Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("lazy-stage-mount: {error:?}");
            ExitCode::FAILURE
        }
    }
}

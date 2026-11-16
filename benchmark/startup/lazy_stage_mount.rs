//! Benchmark-only owner for the runtime's existing public host FUSE image API.
//! Does not implement a filesystem or modify the product execution path.
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// This helper runs inside the Linux owned lab. A small native signal interface
// keeps the existing runtime-rlib link command independent of extra crates.
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
    fn kill(pid: i32, number: i32) -> i32;
}

extern "C" fn interrupted(_: i32) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

struct Task(Child, bool);
impl Drop for Task {
    fn drop(&mut self) {
        if !self.1 {
            unsafe {
                kill(-(self.0.id() as i32), 9);
            }
            let _ = self.0.wait();
        }
    }
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 || args[5] != "--" {
        return Err(
            "usage: lazy-stage-mount SERVER HANDLE STORE RECEIPT -- COMMAND ARGS...".into(),
        );
    }
    let handlers = unsafe {
        [
            signal(2, interrupted as *const () as usize),
            signal(15, interrupted as *const () as usize),
        ]
    };
    if handlers.contains(&usize::MAX) {
        return Err("could not install stage-owner signal handlers".into());
    }
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let starttime = stat
        .rsplit_once(')')
        .ok_or("invalid owner stat")?
        .1
        .split_whitespace()
        .nth(19)
        .ok_or("missing owner start time")?;
    std::fs::write(
        format!("{}.owner", args[4]),
        format!("pid={}\nstarttime={starttime}\n", std::process::id()),
    )?;
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
    let mut task = Task(
        Command::new(&args[6])
            .args(&args[7..])
            .current_dir(&root)
            .process_group(0)
            .spawn()?,
        false,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = task.0.try_wait()? {
            task.1 = true;
            break status;
        }
        if INTERRUPTED.load(Ordering::Relaxed) || Instant::now() >= deadline {
            // Reap the owned CLI group before releasing its FUSE lower.
            drop(task);
            drop(image);
            return Err("stage task interrupted or exceeded owner deadline".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
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

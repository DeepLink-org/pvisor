//! Worker for B-FS-ENG/B-FS-DIAG; real host FUSE, never a simulated mount.
use anyhow::{Context, Result, ensure};
use pvisor_overlay_core::LayerMutability;
use pvisor_overlayfs::api::{
    OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySessionControl,
};
use serde_json::json;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn emit(value: serde_json::Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn paths() -> Vec<PathBuf> {
    (0..32)
        .flat_map(|d| {
            (0..64).map(move |f| {
                let prefix = if d < 16 {
                    format!("d{d:02}")
                } else {
                    format!("d{d:02}/a/b/c/e/f/g/h")
                };
                PathBuf::from(format!("{prefix}/f{f:02}.txt"))
            })
        })
        .collect()
}
fn expected(p: &Path) -> Vec<u8> {
    format!(
        "needle {}\n{}\n",
        p.display(),
        "0123456789abcdef".repeat(32)
    )
    .into_bytes()
}
fn read_checked(root: &Path, p: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(root.join(p))?.read_to_end(&mut bytes)?;
    ensure!(bytes == expected(p), "content mismatch: {}", p.display());
    Ok(bytes)
}
fn pass(root: &Path, metadata: bool, repeats: usize) -> Result<()> {
    for _ in 0..repeats {
        for p in paths() {
            if metadata {
                let m = fs::symlink_metadata(root.join(&p))?;
                ensure!(
                    m.is_file() && m.len() == expected(&p).len() as u64,
                    "metadata mismatch"
                );
            }
            let bytes = read_checked(root, &p)?;
            ensure!(
                bytes.windows(6).filter(|s| *s == b"needle").count() == 1,
                "search mismatch"
            );
        }
    }
    Ok(())
}
fn absent(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => anyhow::bail!("expected ENOENT at {}: {:?}", p.display(), other),
    }
}
fn correctness(root: &Path, upper: &Path, native: bool) -> Result<()> {
    let ps = paths();
    let a = &ps[0];
    let b = &ps[1];
    let c = &ps[2];
    let renamed = a.with_file_name("renamed.txt");
    // Warm successful lower resolutions before upper changes.
    for p in [a, b, c] {
        read_checked(root, p)?;
        fs::metadata(root.join(p))?;
    }
    let mut value = expected(a);
    value.extend_from_slice(b"upper-copyup-visible");
    fs::OpenOptions::new()
        .append(true)
        .open(root.join(a))?
        .write_all(b"upper-copyup-visible")?;
    ensure!(
        fs::read(root.join(a))? == value,
        "copyup did not preserve full contents"
    );
    ensure!(
        fs::metadata(root.join(a))?.len() == value.len() as u64,
        "upper metadata stale"
    );
    fs::rename(root.join(a), root.join(&renamed))?;
    absent(&root.join(a))?;
    ensure!(
        fs::read(root.join(&renamed))? == value,
        "rename not visible"
    );
    fs::remove_file(root.join(b))?;
    absent(&root.join(b))?;
    fs::write(root.join(b), b"recreated-visible")?;
    ensure!(
        fs::read(root.join(b))? == b"recreated-visible",
        "recreate not visible"
    );
    fs::remove_file(root.join(c))?;
    absent(&root.join(c))?;
    // Repeat after kernel TTL too, not just invalidation immediately after mutation.
    std::thread::sleep(Duration::from_millis(1100));
    absent(&root.join(a))?;
    absent(&root.join(c))?;
    ensure!(
        fs::read(root.join(b))? == b"recreated-visible",
        "recreate stale after TTL"
    );
    let names: Vec<_> = fs::read_dir(root.join(a.parent().unwrap()))?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    ensure!(
        !names
            .iter()
            .any(|n| n.starts_with(".wh.") || n == "f00.txt" || n == "f02.txt"),
        "whiteout/removed name leaked"
    );
    ensure!(
        names.iter().any(|n| n == "f01.txt") && names.iter().any(|n| n == "renamed.txt"),
        "upper names missing"
    );
    if !native {
        ensure!(
            fs::read(upper.join(&renamed))? == value,
            "upper rename wrong"
        );
        ensure!(
            fs::read(upper.join(b))? == b"recreated-visible",
            "upper recreate wrong"
        );
        for p in [a, c] {
            absent(&upper.join(p))?;
            ensure!(
                upper
                    .join(p.with_file_name(format!(
                        ".wh.{}",
                        p.file_name().unwrap().to_string_lossy()
                    )))
                    .is_file(),
                "missing whiteout"
            );
        }
        ensure!(
            !upper.join(b.with_file_name(".wh.f01.txt")).exists(),
            "recreate whiteout survived"
        );
        let mut actual = Vec::new();
        fn walk(root: &Path, p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
            for entry in fs::read_dir(root.join(p))? {
                let entry = entry?;
                let rel = p.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    walk(root, &rel, out)?
                } else {
                    out.push(rel)
                }
            }
            Ok(())
        }
        walk(upper, Path::new(""), &mut actual)?;
        actual.retain(|p| p != Path::new(".wh..pvisor-root-metadata"));
        let mut wanted = vec![
            renamed,
            b.clone(),
            a.with_file_name(".wh.f00.txt"),
            c.with_file_name(".wh.f02.txt"),
        ];
        actual.sort();
        wanted.sort();
        ensure!(actual == wanted, "unexpected upper files: {:?}", actual);
    }
    Ok(())
}
fn tools(root: &Path) -> Result<()> {
    let status = Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(root)
        .output()?;
    ensure!(
        status.status.success() && status.stdout.is_empty(),
        "git status failed/dirty: {:?}",
        status
    );
    let rg = Command::new("rg")
        .args([
            "--no-config",
            "--hidden",
            "--glob",
            "!.git/**",
            "--glob",
            "*.txt",
            "--files-with-matches",
            "needle",
            ".",
        ])
        .current_dir(root)
        .output()?;
    ensure!(rg.status.success(), "rg failed: {:?}", rg);
    let mut actual: Vec<_> = String::from_utf8(rg.stdout)?
        .lines()
        .map(|s| s.trim_start_matches("./").to_owned())
        .collect();
    actual.sort();
    let mut wanted: Vec<_> = paths()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    wanted.sort();
    ensure!(actual == wanted, "rg output mismatch");
    pass(root, false, 1)?; // Complete task verifies every byte, not merely a substring.
    Ok(())
}
fn main() -> Result<()> {
    // Finite lifetime even if the coordinator loses its pipes. Coordinator also
    // bounds every response and owns process-group termination/detachment.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(1800));
        std::process::exit(124)
    });
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 4, "usage: driver CONDITION LOWER OWNED-STAGE");
    let condition = &args[1];
    let lower = PathBuf::from(&args[2]);
    let stage = PathBuf::from(&args[3]);
    let upper = stage.join("upper");
    let root = if condition == "native" {
        lower.clone()
    } else {
        stage.join("mnt")
    };
    let start = Instant::now();
    let session = if condition == "native" {
        None
    } else {
        let mut config = OverlayMountConfig::new(
            vec![lower],
            upper.clone(),
            Some(stage.join("work")),
            root.clone(),
        );
        config.default_permissions = true;
        config.preimage_dir = None;
        if condition.starts_with("immutable-") {
            config.lower_mutability = vec![LayerMutability::Immutable];
        }
        Some(
            OverlayFs::mount(config)
                .context("real FUSE mount failed; do not bypass host permissions")?,
        )
    };
    emit(
        json!({"event":"ready","mount_ms":start.elapsed().as_secs_f64()*1000.0,"condition":condition}),
    );
    for line in io::stdin().lock().lines() {
        let line = line?;
        let start = Instant::now();
        match line.as_str() {
            "hot" | "ttl" => pass(&root, true, 2)?,
            "readsearch" => pass(&root, false, 1)?,
            "tools" => tools(&root)?,
            "correct" => correctness(&root, &upper, condition == "native")?,
            "stop" => break,
            _ => anyhow::bail!("unknown command {line}"),
        }
        emit(
            json!({"event":"result","workload":line,"operation_ms":start.elapsed().as_secs_f64()*1000.0,"correctness":"passed"}),
        );
    }
    let start = Instant::now();
    if let Some(session) = session {
        session.unmount()?;
    }
    emit(json!({"event":"stopped","unmount_ms":start.elapsed().as_secs_f64()*1000.0}));
    Ok(())
}

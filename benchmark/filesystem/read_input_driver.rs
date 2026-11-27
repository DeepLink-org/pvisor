//! Benchmark: B-FS-ENG (opt-in engineering mechanism experiment, not a gate).
//! Does a one-time immutable pack import pay back for sparse or repeated POSIX reads?
//! Each block runs all four cells in seeded order, in fresh Rust-native VMs.

use std::{
    fs::{self, File, Permissions},
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

const FILES: usize = 128;
const BLOCKS: usize = 32;
const BLOCK: usize = 1024;

fn fixture(dir: &Path) -> io::Result<()> {
    fs::create_dir(dir)?;
    for i in 0..FILES {
        let file = File::create_new(dir.join(format!("part-{i}")))?;
        let mut out = BufWriter::new(file);
        let first = (31 * i) as u8;
        for j in 0..BLOCKS {
            out.write_all(&[first.wrapping_add(j as u8); BLOCK])?;
        }
        out.flush()?;
        out.get_ref()
            .set_permissions(Permissions::from_mode([0o444, 0o440][i % 2]))?;
    }
    Ok(())
}

fn build_pack(source: &Path, dir: &Path) -> io::Result<()> {
    fs::create_dir(dir)?;
    let mut out = BufWriter::with_capacity(256 * 1024, File::create_new(dir.join("input.pack"))?);
    let mut buf = vec![0; BLOCK * BLOCKS];
    for i in 0..FILES {
        let mut file = File::open(source.join(format!("part-{i}")))?;
        let mode = (file.metadata()?.permissions().mode() & 0o7777) as u16;
        file.read_exact(&mut buf)?;
        assert_eq!(file.read(&mut [0])?, 0, "unexpected source length");
        out.write_all(&mode.to_le_bytes())?;
        out.write_all(&buf)?;
    }
    // The pack is consumed immediately, not published as a crash-durable artifact.
    out.flush()
}

fn oracle(case: &str) -> (usize, u64) {
    let (selected, passes): (Vec<usize>, usize) = match case {
        "sparse" => (vec![0, 43, 85, 127], 1),
        "broad" => ((0..FILES).collect(), 4),
        "audit" => ((0..FILES).collect(), 1),
        _ => unreachable!(),
    };
    let sum: usize = selected
        .iter()
        .flat_map(|i| (0..BLOCKS).map(move |j| (i * 31 + j) % 256))
        .sum();
    (passes * selected.len(), (passes * sum) as u64)
}

fn trial(
    vm: &Path,
    probe: &Path,
    source: &Path,
    dir: &Path,
    arm: &str,
    case: &str,
) -> io::Result<String> {
    fs::create_dir(dir)?;
    let root = dir.join("root");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("dev"))?;
    fs::copy(probe, root.join("probe"))?;
    let packed = dir.join("packed");
    let share = if arm == "pack" { &packed } else { source };
    let mut cmd = Command::new("timeout");
    cmd.env_clear()
        .env("PVISOR_GUEST_KERNEL", "rust-native")
        .env("PVISOR_VM_FS_QUEUE_HANDOFF", "0")
        .args(["-k", "3s", "30s"])
        .arg(vm)
        .arg("--rootfs")
        .arg(&root)
        .arg("--readonly")
        .arg(share)
        .args([
            "--cpus",
            "1",
            "--memory",
            "256",
            "--task-stdio",
            "--",
            "/probe",
            arm,
            case,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    fs::write(dir.join("command"), format!("{cmd:?}\n"))?;
    let mut stderr = File::create_new(dir.join("stderr"))?;
    let mut stdout = File::create_new(dir.join("stdout"))?;

    let start = Instant::now();
    let build_ns = if arm == "pack" {
        build_pack(source, &packed)?;
        start.elapsed().as_nanos()
    } else {
        0
    };
    let launched = Instant::now();
    let mut child = cmd.spawn()?;
    let mut errors = child.stderr.take().unwrap();
    let drain = std::thread::spawn(move || io::copy(&mut errors, &mut stderr));
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    output.read_line(&mut first)?;
    let ready = Instant::now();
    if first.starts_with("RESULT ") {
        child.stdin.take().unwrap().write_all(b"\n")?;
    } else {
        drop(child.stdin.take());
    }
    stdout.write_all(first.as_bytes())?;
    let mut rest = String::new();
    output.read_to_string(&mut rest)?;
    stdout.write_all(rest.as_bytes())?;
    let status = child.wait()?;
    let exit_ns = launched.elapsed().as_nanos();
    drain.join().unwrap()?;
    fs::write(dir.join("status"), format!("{status}\n"))?;
    assert!(
        status.success(),
        "{status}; invalid trial: {}",
        dir.display()
    );

    let fields: Vec<&str> = first.split_whitespace().collect();
    assert!(
        fields.len() == 5 && fields[0] == "RESULT",
        "invalid result: {first}"
    );
    let import_ns: u64 = fields[1].parse().unwrap();
    let work_ns: u64 = fields[2].parse().unwrap();
    assert_eq!(arm == "pack", import_ns != 0);
    assert!(work_ns > 0);
    let (files, sum) = oracle(case);
    assert_eq!(
        (fields[3], fields[4]),
        (&*files.to_string(), &*sum.to_string())
    );
    let (files, sum) = oracle("audit");
    assert_eq!(rest, format!("AUDIT {files} {sum}\n"));
    Ok(format!(
        "{build_ns},{import_ns},{work_ns},{},{},{exit_ns}",
        ready.duration_since(launched).as_nanos(),
        ready.duration_since(start).as_nanos()
    ))
}

fn shuffle(items: &mut [(&str, &str)], seed: &mut u64) {
    for i in (1..items.len()).rev() {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        items.swap(i, (*seed % (i + 1) as u64) as usize);
    }
}

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(args.len(), 5, "VM PROBE NEW_DIR BLOCKS SEED");
    let vm = Path::new(&args[0]).canonicalize()?;
    let probe = Path::new(&args[1]).canonicalize()?;
    let dir = PathBuf::from(&args[2]);
    let blocks: usize = args[3].to_str().unwrap().parse().unwrap();
    let mut seed: u64 = args[4].to_str().unwrap().parse().unwrap();
    assert!(
        (1..=32).contains(&blocks) && seed != 0,
        "1..32 complete blocks; nonzero seed"
    );
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let source = dir.join("input");
    fixture(&source)?;
    fs::write(
        dir.join("plan"),
        format!("blocks={blocks}\nseed={seed}\nvm={vm:?}\nprobe={probe:?}\n"),
    )?;
    let mut csv = File::create_new(dir.join("samples.csv"))?;
    writeln!(
        csv,
        "block,order,arm,case,build_ns,import_ns,work_ns,vm_result_ns,cold_result_ns,vm_including_audit_ns"
    )?;
    for block in 1..=blocks {
        let mut order = [
            ("direct", "sparse"),
            ("pack", "sparse"),
            ("direct", "broad"),
            ("pack", "broad"),
        ];
        shuffle(&mut order, &mut seed);
        for (slot, (arm, case)) in order.into_iter().enumerate() {
            let cell = dir.join(format!("{block:02}-{slot}-{arm}-{case}"));
            let row = trial(&vm, &probe, &source, &cell, arm, case)?;
            writeln!(csv, "{block},{slot},{arm},{case},{row}")?;
        }
    }
    Ok(())
}

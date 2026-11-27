//! B-VM-GUEST-ENG: one complete operation; no sampling or product entry point.

use std::{
    env, fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Instant,
};

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn owner_mode(meta: &fs::Metadata, mode: u32) {
    assert_eq!(
        (meta.uid(), meta.gid(), meta.mode() & 0o7777),
        (1000, 1000, mode)
    );
}

fn verify(ready: &Path, check_guest_metadata: bool) {
    let dir = ready.symlink_metadata().unwrap();
    assert!(dir.is_dir());
    if check_guest_metadata {
        owner_mode(&dir, 0o700);
    }
    assert_eq!(fs::read_dir(ready.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(ready).unwrap().count(), 8);
    for i in 0..8 {
        let path = ready.join(format!("part-{i}"));
        let meta = path.symlink_metadata().unwrap();
        assert!(meta.is_file() && meta.len() == 512 * 1024);
        if check_guest_metadata {
            owner_mode(&meta, 0o600);
            assert_eq!(meta.nlink(), 1);
        }
        for (j, block) in fs::read(path).unwrap().chunks_exact(1024).enumerate() {
            assert!(block.iter().all(|&x| x == (i * 31 + j) as u8));
        }
    }
}

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    assert!(
        matches!(args.len(), 4 | 5),
        "VM PROBE shared|local|pack NEW_TRIAL_DIR [WRITE_KIB]"
    );
    let vm = Path::new(&args[0]).canonicalize().unwrap();
    let probe = Path::new(&args[1]).canonicalize().unwrap();
    let mode = args[2].to_str().unwrap();
    assert!(matches!(mode, "shared" | "local" | "pack"));
    let chunk = args.get(4).map(|arg| arg.to_str().unwrap());
    if let Some(chunk) = chunk {
        assert!(mode != "pack" && matches!(chunk, "1" | "16" | "256"));
    }
    let trial = PathBuf::from(&args[3]);
    let root = trial.join("root");
    let out = root.join("output");
    let result = trial.join("result");

    let start = Instant::now();
    fs::DirBuilder::new().mode(0o700).create(&trial).unwrap();
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("dev")).unwrap();
    fs::create_dir(&out).unwrap();
    fs::set_permissions(&out, fs::Permissions::from_mode(0o777)).unwrap();
    fs::copy(&probe, root.join("probe")).unwrap();
    let pack = (mode == "pack").then(|| {
        fs::create_dir(&result).unwrap();
        let path = out.join("pack");
        let f = fs::File::create_new(&path).unwrap();
        f.set_permissions(fs::Permissions::from_mode(0o666))
            .unwrap();
        fs::File::open(path).unwrap()
    });
    let setup = start.elapsed();

    let output = success(
        Command::new(vm)
            .env("PVISOR_GUEST_KERNEL", "rust-native")
            .arg("--rootfs")
            .arg(&root)
            .args(["--cpus", "1", "--memory", "256", "--task-stdio"])
            .args(["--", "/probe", mode])
            .args(chunk)
            .stdin(Stdio::piped())
            .output()
            .unwrap(),
    );
    let exited = start.elapsed();
    let (ready, guest_ns) = if let Some(pack) = pack {
        assert_eq!(output, b"PACK_SUBMITTED\n");
        success(
            Command::new(probe)
                .args(["import", "0"])
                .stdin(pack)
                .current_dir(&result)
                .output()
                .unwrap(),
        );
        (result.join("ready"), None)
    } else {
        let marker = std::str::from_utf8(&output).unwrap();
        let prefix = chunk.map_or_else(
            || "TREE_PUBLISHED_NS ".into(),
            |size| format!("TREE_PUBLISHED {mode} {size} "),
        );
        let ns = marker
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix('\n'))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(ns > 0 && u128::from(ns) <= (exited - setup).as_nanos());
        (out.join("ready"), Some(ns))
    };
    let end = start.elapsed();
    verify(&ready, chunk.is_some());
    let write_kib = chunk.unwrap_or("1");
    let guest_ns = guest_ns.map_or_else(|| "null".into(), |ns| ns.to_string());
    println!(
        r#"{{"arm":"{mode}","write_kib":{write_kib},"guest_publish_ns":{guest_ns},"end_to_end_ns":{},"setup_ns":{},"vm_ns":{},"finish_ns":{}}}"#,
        end.as_nanos(),
        setup.as_nanos(),
        (exited - setup).as_nanos(),
        (end - exited).as_nanos()
    );
}

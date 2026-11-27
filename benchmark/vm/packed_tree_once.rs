//! B-VM-GUEST-ENG: one complete operation; no sampling or product entry point.

use std::{
    env, fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
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

fn verify(ready: &Path) {
    assert!(ready.symlink_metadata().unwrap().is_dir());
    assert_eq!(fs::read_dir(ready).unwrap().count(), 8);
    for i in 0..8 {
        let path = ready.join(format!("part-{i}"));
        let meta = path.symlink_metadata().unwrap();
        assert!(meta.is_file() && meta.len() == 512 * 1024);
        for (j, block) in fs::read(path).unwrap().chunks_exact(1024).enumerate() {
            assert!(block.iter().all(|&x| x == (i * 31 + j) as u8));
        }
    }
}

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    assert_eq!(args.len(), 4, "VM PROBE shared|local|pack NEW_TRIAL_DIR");
    let vm = Path::new(&args[0]).canonicalize().unwrap();
    let probe = Path::new(&args[1]).canonicalize().unwrap();
    let mode = args[2].to_str().unwrap();
    assert!(matches!(mode, "shared" | "local" | "pack"));
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
            .stdin(Stdio::piped())
            .output()
            .unwrap(),
    );
    let exited = start.elapsed();
    let ready = if let Some(pack) = pack {
        assert_eq!(output, b"PACK_SUBMITTED\n");
        success(
            Command::new(probe)
                .args(["import", "0"])
                .stdin(pack)
                .current_dir(&result)
                .output()
                .unwrap(),
        );
        result.join("ready")
    } else {
        let marker = std::str::from_utf8(&output).unwrap();
        marker
            .strip_prefix("TREE_PUBLISHED_NS ")
            .and_then(|s| s.strip_suffix('\n'))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        out.join("ready")
    };
    let end = start.elapsed();
    verify(&ready);
    println!(
        r#"{{"arm":"{mode}","end_to_end_ns":{},"setup_ns":{},"vm_ns":{},"finish_ns":{}}}"#,
        end.as_nanos(),
        setup.as_nanos(),
        (exited - setup).as_nanos(),
        (end - exited).as_nanos()
    );
}

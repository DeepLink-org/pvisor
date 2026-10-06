//! Native execution checkpoint RAM helper regressions.
use std::process::Command;

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn ram_helper_failure_keeps_mount_and_spec_out_of_durable_state() {
    use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
    use std::{
        fs,
        io::Write,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };
    const ROLE: &str = "PVISOR_RAM_PLACEMENT_TEST";
    if let Some(root) = std::env::var_os(ROLE) {
        let root = std::path::PathBuf::from(root);
        let source = root.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let store_root = root.join("home/snapshots");
        let store = SnapshotStore::new(&store_root).unwrap();
        let compatibility = Compatibility {
            host_boot: "test".into(),
            build: "test".into(),
            firmware: "test".into(),
            profile: "test".into(),
        };
        let pending = store.begin().unwrap();
        pending
            .create_ram()
            .unwrap()
            .write_all(b"persistent RAM")
            .unwrap();
        let id = pending
            .publish(&source, b"machine", compatibility.clone())
            .unwrap();
        let published = store.open_for_restore(&id, &compatibility).unwrap();
        let legacy_parent = store_root.join("ram-mounts");
        // Neither the old parent nor TMPDIR needs to exist anymore.
        for ready in [false, true] {
            let record = root.join("record.json");
            let helper = root.join("helper");
            fs::write(&helper, format!(
                "#!/usr/bin/env python3\nimport os,json,stat,sys\np=os.environ['PVISOR_VM_RESTORE_RAM_SERVER']\ns=json.load(open(p))\nm=os.fsdecode(bytes(s['mount']))\njson.dump(dict(spec=p,mount=m,spec_mode=stat.S_IMODE(os.stat(p).st_mode),parent_mode=stat.S_IMODE(os.stat(os.path.dirname(p)).st_mode),mount_mode=stat.S_IMODE(os.stat(m).st_mode),uid=os.stat(m).st_uid),open({},'w'))\n{}\nsys.exit(1)\n",
                serde_json::to_string(&record).unwrap(),
                if ready { "print('ready',flush=True)" } else { "" },
            )).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(published.ram_mount(&helper, &legacy_parent).is_err());
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
            let spec = std::path::Path::new(record["spec"].as_str().unwrap());
            let mount = std::path::Path::new(record["mount"].as_str().unwrap());
            assert!(!mount.starts_with(&root));
            assert!(!spec.starts_with(&root));
            assert_eq!(record["spec_mode"], 0o600);
            assert_eq!(record["parent_mode"], 0o700);
            assert_eq!(record["mount_mode"], 0o700);
            assert_eq!(record["uid"], fs::metadata(&root).unwrap().uid());
            assert!(fs::symlink_metadata(mount).is_err());
            assert!(fs::symlink_metadata(spec.parent().unwrap()).is_err());
            assert!(!legacy_parent.exists());
            assert!(!root.join("redirected-tmp").exists());
            let mut reader = published.ram_reader().unwrap();
            let mut bytes = [0; 14];
            reader.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"persistent RAM");
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ram_helper_failure_keeps_mount_and_spec_out_of_durable_state",
            "--nocapture",
        ])
        .env(ROLE, root.path())
        .env("HOME", root.path().join("home"))
        .env("TMPDIR", root.path().join("redirected-tmp"))
        .env("XDG_RUNTIME_DIR", root.path().join("home"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn ram_watchdog_cleans_an_already_detached_private_directory() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("ram-mount-watchdog-test");
    std::fs::create_dir(&mount).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env(
            "PVISOR_VM_RESTORE_RAM_WATCHDOG",
            mount.canonicalize().unwrap(),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!mount.exists());
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
#[ignore = "real FUSE pager gate, no VMs"]
fn external_ram_pager_survives_object_deletion() {
    use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
    use std::{
        fs,
        io::{Read, Write},
    };
    for compressed in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&temporary.path().join("store")).unwrap();
        let compatibility = Compatibility {
            host_boot: "test".into(),
            build: "test".into(),
            firmware: "test".into(),
            profile: "test".into(),
        };
        let bytes: Vec<_> = (0..131072).map(|offset| (offset % 251) as u8).collect();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(&bytes).unwrap();
        let id = if compressed {
            pending
                .publish_compressed(&source, b"machine", compatibility.clone())
                .unwrap()
        } else {
            pending
                .publish(&source, b"machine", compatibility.clone())
                .unwrap()
        };
        let published = store.open_for_restore(&id, &compatibility).unwrap();
        let mounts = temporary.path().join("mounts");
        fs::create_dir(&mounts).unwrap();
        let (mount, mut file) = published
            .ram_mount(std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")), &mounts)
            .unwrap();
        #[cfg(target_os = "linux")]
        let mount_path = {
            use std::os::fd::AsRawFd;
            let path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
            let parent = path.parent().unwrap().to_path_buf();
            assert!(!parent.starts_with(temporary.path()));
            assert!(!parent.starts_with(&source));
            assert!(!parent.starts_with(temporary.path().join("store")));
            parent
        };
        drop(published);
        store.delete(&id).unwrap();
        store.collect_abandoned().unwrap();
        let mut output = Vec::new();
        file.read_to_end(&mut output).unwrap();
        assert_eq!(output, bytes);
        drop(file);
        drop(mount);
        #[cfg(target_os = "linux")]
        assert!(
            fs::symlink_metadata(&mount_path).is_err(),
            "runtime mount directory survived normal release"
        );
        assert_eq!(fs::read_dir(&mounts).unwrap().count(), 0);
    }
}

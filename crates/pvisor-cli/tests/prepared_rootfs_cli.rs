//! Local physical-rootfs preparation and normal Host-worker admission boundaries.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
use pvisor::environment_snapshot::SnapshotStore;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn command(binary: &str, root: &Path) -> Command {
    for directory in ["home", "config", "cache", "data", "state", "runtime"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    #[cfg(target_os = "linux")]
    let binary = if std::env::var_os("PVISOR_PREPARED_HOST_CASE").is_some() {
        if binary == env!("CARGO_BIN_EXE_pvisor") {
            "/bin/pvisor"
        } else {
            "/bin/pvisor-cache"
        }
    } else {
        binary
    };
    let mut command = Command::new(binary);
    command
        .env_clear()
        .current_dir(root)
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("PVISOR_RUN_HOME", root.join("jobs"))
        .env("PVISOR_STARTUP_TIMING", "0");
    command
}

// HOME/XDG/RUN_HOME do not isolate the fixed /tmp/pvisor-host-UID endpoint.
// Re-enter only these real Host cases with private user/mount/PID namespaces.
// The test is PID 1: its exit also destroys its own listener, without signalling
// any process or modifying any generation in the caller's /tmp namespace.
#[cfg(target_os = "linux")]
fn isolated_host_case(name: &str) -> bool {
    const ROLE: &str = "PVISOR_PREPARED_HOST_CASE";
    if std::env::var(ROLE).as_deref() == Ok(name) {
        let mounted = unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                c"/tmp".as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=1777,size=64m".as_ptr().cast(),
            )
        };
        assert_eq!(
            mounted,
            0,
            "mount private test /tmp: {}",
            std::io::Error::last_os_error()
        );
        // A rootless user namespace maps host-root ancestors to overflow UID.
        // Keep the production executable-ancestor checks intact by using a
        // fully owned root and copied CLI inodes inside it. Libraries are
        // read-only binds; /proc describes this PID namespace, not the host.
        assert_eq!(
            unsafe { libc::getpid() },
            1,
            "Host fixture must own its PID namespace"
        );
        let owned =
            std::path::PathBuf::from(std::env::var_os("PVISOR_PREPARED_HOST_ROOT").unwrap());
        fs::create_dir(owned.join("tmp")).unwrap();
        for source in ["/lib", "/lib64", "/usr", "/etc", "/proc", "/dev"] {
            if !Path::new(source).is_dir() {
                continue;
            }
            let target = owned.join(source.trim_start_matches('/'));
            fs::create_dir(&target).unwrap();
            let source = std::ffi::CString::new(source).unwrap();
            let target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        source.as_ptr(),
                        target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REC,
                        std::ptr::null(),
                    )
                },
                0,
                "bind fixture runtime {} -> {}: {}",
                source.to_string_lossy(),
                target.to_string_lossy(),
                std::io::Error::last_os_error()
            );
            let no_devices = if source.as_bytes() == b"/dev" {
                // A surrounding sandbox can lock NODEV on /dev while
                // supplying usable device files as nested mounts. Preserve
                // that flag instead of attempting to clear it in a userns.
                let mut filesystem = unsafe { std::mem::zeroed::<libc::statvfs>() };
                assert_eq!(
                    unsafe { libc::statvfs(source.as_ptr(), &mut filesystem) },
                    0
                );
                if filesystem.f_flag & libc::ST_NODEV != 0 {
                    libc::MS_NODEV
                } else {
                    0
                }
            } else {
                libc::MS_NODEV
            };
            let no_exec = if [b"/dev".as_slice(), b"/proc".as_slice(), b"/etc".as_slice()]
                .contains(&source.as_bytes())
            {
                libc::MS_NOEXEC
            } else {
                0
            };
            assert_eq!(
                unsafe {
                    libc::mount(
                        std::ptr::null(),
                        target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND
                            | libc::MS_REMOUNT
                            | libc::MS_RDONLY
                            | libc::MS_NOSUID
                            | no_devices
                            | no_exec,
                        std::ptr::null(),
                    )
                },
                0,
                "make fixture runtime {} read-only: {}",
                source.to_string_lossy(),
                std::io::Error::last_os_error()
            );
        }
        let owned = std::ffi::CString::new(owned.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::chroot(owned.as_ptr()) },
            0,
            "enter owned fixture root: {}",
            std::io::Error::last_os_error()
        );
        std::env::set_current_dir("/").unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    c"tmpfs".as_ptr(),
                    c"/tmp".as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"mode=1777,size=64m".as_ptr().cast(),
                )
            },
            0,
            "mount chroot private /tmp: {}",
            std::io::Error::last_os_error()
        );
        return true;
    }
    // Debug binaries can be large. Own actual copies on disk, not in the
    // fixture's small tmpfs; the outer process removes them after PID 1 exits.
    let owned = tempfile::Builder::new()
        .prefix(".prepared-host-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    fs::create_dir(owned.path().join("bin")).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_pvisor"),
        owned.path().join("bin/pvisor"),
    )
    .unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_pvisor-cache"),
        owned.path().join("bin/pvisor-cache"),
    )
    .unwrap();
    let output = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child",
            "--mount-proc",
            "--propagation",
            "private",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(ROLE, name)
        .env("PVISOR_PREPARED_HOST_ROOT", owned.path())
        .output()
        .expect("run isolated prepared-rootfs Host fixture with util-linux unshare");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if std::env::var_os("PVISOR_TEST_ALLOW_NO_USERNS").is_some()
        && (stderr.trim() == "unshare: unshare failed: Operation not permitted"
            || stderr.trim() == "unshare: unshare failed: Permission denied")
    {
        eprintln!("skipping explicitly optional prepared-rootfs Host namespace: {stderr}");
        return false;
    }
    assert!(
        output.status.success(),
        "isolated {name}: {stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    false
}

fn prepare(root: &Path) -> pvisor::PreparedRootfs {
    fs::create_dir(root.join("source")).unwrap();
    fs::write(root.join("source/file"), b"owned").unwrap();
    let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), root)
        .args(["prepare-rootfs", "--rootfs"])
        .arg(root.join("source"))
        .arg("--base-store")
        .arg(root.join("store"))
        .env("PVISOR_CACHE_SERVER", "unix:///does-not-exist.sock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    serde_json::from_value(value["prepared_rootfs"].clone()).unwrap()
}

fn run(root: &Path, args: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_pvisor"), root)
        .args(["run", "--no-agent-defaults"])
        .args(args)
        .output()
        .unwrap()
}

fn prepare_workspace(root: &Path) -> pvisor::PreparedBase {
    fs::create_dir(root.join("workspace-source")).unwrap();
    fs::write(root.join("workspace-source/file"), b"fixed workspace").unwrap();
    let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), root)
        .args(["prepare-workspace", "--workspace"])
        .arg(root.join("workspace-source"))
        .arg("--base-store")
        .arg(root.join("workspace-store"))
        .env("PVISOR_CACHE_SERVER", "unix:///does-not-exist.sock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(value.get("prepared_rootfs").is_none());
    let selected: pvisor::PreparedBase =
        serde_json::from_value(value["prepared_workspace"].clone()).unwrap();
    let held = SnapshotStore::new(&selected.store)
        .unwrap()
        .open_prepared_workspace(&selected.base)
        .unwrap();
    assert_eq!(
        value["workspace"],
        serde_json::to_value(held.root()).unwrap()
    );
    selected
}

#[test]
fn local_workspace_prepare_is_persistent_purpose_bound_and_rejects_overlap() {
    let root = tempfile::tempdir().unwrap();
    let selected = prepare_workspace(root.path());
    fs::remove_dir_all(root.path().join("workspace-source")).unwrap();
    let store = SnapshotStore::new(&selected.store).unwrap();
    store.collect_abandoned().unwrap();
    let base = store.open_prepared_workspace(&selected.base).unwrap();
    assert_eq!(
        fs::read(base.root().join("file")).unwrap(),
        b"fixed workspace"
    );
    assert!(store.open_prepared_rootfs(&selected.base).is_err());
    let nested = root.path().join("nested-workspace-store");
    let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), root.path())
        .arg("prepare-workspace")
        .arg("--workspace")
        .arg(root.path())
        .arg("--base-store")
        .arg(&nested)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("independent trees"));
    assert!(!nested.exists());
}

#[test]
fn workspace_selector_requires_pair_before_host_admission() {
    let root = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (vec!["--workspace-base", "a"], "--workspace-base-store"),
        (
            vec!["--workspace-base-store", "/missing"],
            "--workspace-base",
        ),
    ] {
        let output = run(root.path(), &args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("required arguments were not provided") && stderr.contains(expected),
            "{stderr}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn workspace_worker_rejects_wrong_purpose_and_json_spec_in_private_namespace() {
    if !isolated_host_case(
        "workspace_worker_rejects_wrong_purpose_and_json_spec_in_private_namespace",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let rootfs = prepare(root.path());
    let workspace = prepare_workspace(root.path());
    let live = root.path().join("live");
    fs::create_dir(&live).unwrap();
    let root_store = rootfs.store.to_str().unwrap();
    let workspace_store = workspace.store.to_str().unwrap();
    // Substitute an authentic rootfs-only record into the workspace catalog:
    // purpose, rather than a digest-looking id or valid seal, must authorize it.
    let store = SnapshotStore::new(&workspace.store).unwrap();
    let base = store.open_prepared_workspace(&workspace.base).unwrap();
    store.retain_prepared_rootfs(&base).unwrap();
    let destination = workspace
        .store
        .join("prepared-workspace")
        .join(&workspace.base.id);
    fs::copy(
        workspace
            .store
            .join("prepared-rootfs")
            .join(&workspace.base.id)
            .join("record.json"),
        destination.join("record.json"),
    )
    .unwrap();
    let output = run(
        &live,
        &[
            "--vm",
            "--rootfs-base-store",
            root_store,
            "--rootfs-base",
            &rootfs.base.id,
            "--workspace-base-store",
            workspace_store,
            "--workspace-base",
            &workspace.base.id,
            "--",
            "true",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("authenticate prepared workspace catalog") && stderr.contains("purpose"),
        "{stderr}"
    );
    let spec = pvisor_core::RunSpec::process("workspace-spec-rejection", "test", "/bin/true");
    fs::write(live.join("spec.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
    let output = run(
        &live,
        &[
            "--spec",
            "spec.json",
            "--workspace-base-store",
            workspace_store,
            "--workspace-base",
            &workspace.base.id,
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("JSON --spec supports only host"));
    let authority = pvisor::host_transport::host_authority_root().unwrap();
    let generation_before = fs::read(authority.join("generation")).unwrap();
    let private_source = authority.join("private-workspace-input");
    fs::create_dir(&private_source).unwrap();
    fs::write(private_source.join("secret"), b"fixture authority secret").unwrap();
    for (source, destination) in [
        (
            private_source,
            root.path().join("exfiltrated-workspace-store"),
        ),
        (
            root.path().join("workspace-source"),
            authority.join("forbidden-workspace-store"),
        ),
    ] {
        let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), root.path())
            .arg("prepare-workspace")
            .arg("--workspace")
            .arg(source)
            .arg("--base-store")
            .arg(&destination)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("exposes private host service state")
        );
        assert!(!destination.exists());
    }
    assert_eq!(
        fs::read(authority.join("generation")).unwrap(),
        generation_before
    );
}

#[test]
fn local_prepare_survives_cli_exit_and_refuses_source_store_overlap() {
    let temp = tempfile::tempdir().unwrap();
    let selected = prepare(temp.path());
    fs::remove_dir_all(temp.path().join("source")).unwrap();
    let store = SnapshotStore::new(&selected.store).unwrap();
    store.collect_abandoned().unwrap();
    assert_eq!(
        fs::read(
            store
                .open_prepared_rootfs(&selected.base)
                .unwrap()
                .root()
                .join("file")
        )
        .unwrap(),
        b"owned"
    );
    let nested = temp.path().join("nested-store");
    let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), temp.path())
        .arg("prepare-rootfs")
        .arg("--rootfs")
        .arg(temp.path())
        .arg("--base-store")
        .arg(&nested)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("independent trees"));
    assert!(
        !nested.exists(),
        "overlap is rejected before initializing the destination"
    );
}

#[test]
fn prepared_selector_requires_pair_and_rejects_source_conflict_before_host_admission() {
    let temp = tempfile::tempdir().unwrap();
    let id = "a".repeat(64);
    for (args, expected) in [
        (vec!["--rootfs-base", id.as_str()], "--rootfs-base-store"),
        (vec!["--rootfs-base-store", "/missing"], "--rootfs-base"),
        (
            vec![
                "--rootfs",
                "host",
                "--rootfs-base-store",
                "/missing",
                "--rootfs-base",
                id.as_str(),
                "--",
                "true",
            ],
            "cannot be used with",
        ),
    ] {
        let output = run(temp.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "must fail in Clap before Host admission: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        if !args.contains(&"--rootfs") {
            assert!(
                stderr.contains("required arguments were not provided"),
                "{stderr}"
            );
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn prepared_selector_rejects_host_container_and_json_spec_in_private_namespace() {
    if !isolated_host_case(
        "prepared_selector_rejects_host_container_and_json_spec_in_private_namespace",
    ) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let selected = prepare(temp.path());
    let id = selected.base.id;
    let store = selected.store.to_str().unwrap();
    for (args, expected) in [
        (
            vec![
                "--executor",
                "host",
                "--rootfs-base-store",
                store,
                "--rootfs-base",
                id.as_str(),
                "--",
                "true",
            ],
            "prepared rootfs requires the VM executor",
        ),
        (
            vec![
                "--executor",
                "container",
                "--rootfs-base-store",
                store,
                "--rootfs-base",
                id.as_str(),
                "--",
                "true",
            ],
            "prepared rootfs cannot be combined with container execution",
        ),
    ] {
        let output = run(temp.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }
    let spec = pvisor_core::RunSpec::process("prepared-spec-rejection", "test", "/bin/true");
    fs::write(
        temp.path().join("spec.json"),
        serde_json::to_vec(&spec).unwrap(),
    )
    .unwrap();
    let output = run(
        temp.path(),
        &[
            "--spec",
            "spec.json",
            "--result-file",
            "result.json",
            "--rootfs-base-store",
            store,
            "--rootfs-base",
            &id,
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON --spec supports only host"),
        "{stderr}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn normal_host_worker_authenticates_catalog_before_vm_start_and_rejects_exposure() {
    if !isolated_host_case(
        "normal_host_worker_authenticates_catalog_before_vm_start_and_rejects_exposure",
    ) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let selected = prepare(temp.path());
    // Caller cwd contains the store: this would expose private catalog/data
    // through the separate workspace mount even though rootfs itself is sealed.
    let output = run(
        temp.path(),
        &[
            "--vm",
            "--rootfs-base-store",
            selected.store.to_str().unwrap(),
            "--rootfs-base",
            &selected.base.id,
            "--",
            "true",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("independent of workspace/stage/guest sources")
    );
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let pin = selected
        .store
        .join("prepared-rootfs")
        .join(&selected.base.id)
        .join("reference");
    fs::remove_file(&pin).unwrap();
    fs::write(&pin, b"substituted marker").unwrap();
    let output = run(
        &workspace,
        &[
            "--vm",
            "--rootfs-base-store",
            selected.store.to_str().unwrap(),
            "--rootfs-base",
            &selected.base.id,
            "--",
            "true",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("base reference mismatch"));

    // Local cache preparation does not enter a Host worker. Its source/store
    // checks must still protect the canonical authority before any copy/init.
    let authority = pvisor::host_transport::host_authority_root().unwrap();
    let generation_before = fs::read(authority.join("generation")).unwrap();
    let secret_source = authority.join("private-rootfs-input");
    fs::create_dir(&secret_source).unwrap();
    fs::write(
        secret_source.join("private-record"),
        b"test-only private record",
    )
    .unwrap();
    for (source, destination) in [
        (secret_source, temp.path().join("exfiltrated-store")),
        (
            temp.path().join("source"),
            authority.join("forbidden-store"),
        ),
    ] {
        let output = command(env!("CARGO_BIN_EXE_pvisor-cache"), &workspace)
            .arg("prepare-rootfs")
            .arg("--rootfs")
            .arg(&source)
            .arg("--base-store")
            .arg(&destination)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("exposes private host service state"),
            "{stderr}"
        );
        assert!(
            !destination.exists(),
            "authority is rejected before initializing the destination"
        );
    }
    assert_eq!(
        fs::read(authority.join("generation")).unwrap(),
        generation_before
    );
}

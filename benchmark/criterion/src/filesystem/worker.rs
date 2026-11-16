//! Unmeasured fixture preparation and isolated single-invocation Rust subjects.
//! Build as the `filesystem-worker` binary; never runs a sampling loop.
#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, bail, ensure};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeSet,
        ffi::CString,
        fs::{self, File},
        io::Write,
        os::unix::{ffi::OsStrExt, fs::symlink, process::CommandExt},
        path::{Path, PathBuf},
        process::Command,
    };
    const PATTERN: &[u8] = b"pvisor-fixture-needle\n";
    fn chunk() -> Vec<u8> {
        (0..1 << 20).map(|i| i as u8).collect()
    }
    fn expected() -> Value {
        let mut hash = Sha256::new();
        let chunk = chunk();
        for _ in 0..64 {
            hash.update(&chunk);
        }
        json!({"directories":32,"files":2048,"tree_bytes":2048*PATTERN.len(),
        "payload_bytes":64<<20,"payload_sha256":format!("{:x}",hash.finalize())})
    }
    fn inventory(root: &Path) -> Result<BTreeSet<PathBuf>> {
        let mut result = BTreeSet::new();
        fn visit(base: &Path, path: &Path, output: &mut BTreeSet<PathBuf>) -> Result<()> {
            for entry in fs::read_dir(path)? {
                let path = entry?.path();
                let meta = fs::symlink_metadata(&path)?;
                ensure!(!meta.is_symlink(), "fixture symlink");
                output.insert(path.strip_prefix(base)?.to_owned());
                if meta.is_dir() {
                    visit(base, &path, output)?;
                }
            }
            Ok(())
        }
        visit(root, root, &mut result)?;
        Ok(result)
    }
    fn prepare(root: &Path) -> Result<()> {
        fs::create_dir(root)?;
        fs::create_dir(root.join("tree"))?;
        for i in 0..32 {
            fs::create_dir(root.join(format!("tree/d{i:02}")))?;
        }
        for i in 0..2048 {
            fs::write(
                root.join(format!("tree/d{:02}/f{i:06}.txt", i % 32)),
                PATTERN,
            )?;
        }
        let mut file = File::create(root.join("payload.bin"))?;
        let bytes = chunk();
        for _ in 0..64 {
            file.write_all(&bytes)?;
        }
        fs::write(root.join("fixture.json"), serde_json::to_vec(&expected())?)?;
        validate(root)
    }
    fn validate(root: &Path) -> Result<()> {
        ensure!(
            !fs::symlink_metadata(root)?.is_symlink(),
            "fixture root symlink"
        );
        let names = inventory(root)?;
        let mut wanted: BTreeSet<_> = ["tree", "payload.bin", "fixture.json"]
            .iter()
            .map(PathBuf::from)
            .collect();
        for i in 0..32 {
            wanted.insert(PathBuf::from(format!("tree/d{i:02}")));
        }
        for i in 0..2048 {
            let name = PathBuf::from(format!("tree/d{:02}/f{i:06}.txt", i % 32));
            ensure!(fs::read(root.join(&name))? == PATTERN, "small file changed");
            wanted.insert(name);
        }
        ensure!(names == wanted, "fixture inventory changed");
        let fixture: Value = serde_json::from_slice(&fs::read(root.join("fixture.json"))?)?;
        ensure!(fixture == expected(), "fixture declaration changed");
        read(root)
    }
    fn read(root: &Path) -> Result<()> {
        // Retain the original complete read and checksum inside the subject clock.
        let fixture: Value = serde_json::from_slice(&fs::read(root.join("fixture.json"))?)?;
        let bytes = fs::read(root.join("payload.bin"))?;
        ensure!(
            bytes.len() as u64
                == fixture["payload_bytes"]
                    .as_u64()
                    .context("payload length")?
                && format!("{:x}", Sha256::digest(&bytes))
                    == fixture["payload_sha256"]
                        .as_str()
                        .context("payload checksum")?,
            "payload changed"
        );
        Ok(())
    }
    fn metadata(root: &Path) -> Result<()> {
        let fixture: Value = serde_json::from_slice(&fs::read(root.join("fixture.json"))?)?;
        let mut bytes = 0u64;
        let mut files = 0;
        let dirs: Vec<_> = fs::read_dir(root.join("tree"))?.collect::<std::io::Result<_>>()?;
        ensure!(
            dirs.len() as u64 == fixture["directories"].as_u64().context("directory count")?,
            "directory count"
        );
        for dir in dirs {
            ensure!(dir.file_type()?.is_dir(), "not a directory");
            for entry in fs::read_dir(dir.path())? {
                let entry = entry?;
                ensure!(entry.file_type()?.is_file(), "not regular");
                files += 1;
                bytes += entry.metadata()?.len();
            }
        }
        ensure!(
            fixture["files"] == files && fixture["tree_bytes"] == bytes,
            "metadata mismatch"
        );
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn mount(
        source: Option<&Path>,
        target: &Path,
        kind: Option<&str>,
        flags: libc::c_ulong,
        options: Option<&str>,
    ) -> Result<()> {
        let source = source
            .map(|p| CString::new(p.as_os_str().as_bytes()))
            .transpose()?;
        let target = CString::new(target.as_os_str().as_bytes())?;
        let kind = kind.map(CString::new).transpose()?;
        let options = options.map(CString::new).transpose()?;
        ensure!(
            unsafe {
                libc::mount(
                    source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                    target.as_ptr(),
                    kind.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                    flags,
                    options
                        .as_ref()
                        .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
                )
            } == 0,
            "mount: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn namespace(config: &Path) -> Result<()> {
        let config: Value = serde_json::from_slice(&fs::read(config)?)?;
        let string =
            |key: &str| -> Result<&str> { config[key].as_str().with_context(|| key.to_owned()) };
        let root = Path::new(string("namespace_root")?);
        let toolroot = Path::new(string("toolroot")?);
        let folder = Path::new(string("folder")?);
        let mut identities = serde_json::Map::new();
        for name in ["user", "mnt", "pid"] {
            let value = fs::read_link(format!("/proc/self/ns/{name}"))?
                .to_string_lossy()
                .into_owned();
            ensure!(
                config["parent_namespaces"][name] != value,
                "namespace {name} was not replaced"
            );
            identities.insert(name.into(), json!(value));
        }
        let reserved = [
            "tmp",
            "run",
            "proc",
            "dev",
            "sys",
            "bench-trial",
            ".old-root",
        ];
        mount(
            None,
            Path::new("/"),
            None,
            libc::MS_PRIVATE | libc::MS_REC,
            None,
        )?;
        for entry in fs::read_dir(toolroot)? {
            let entry = entry?;
            let name = entry.file_name();
            if reserved.iter().any(|r| name == *r) {
                continue;
            }
            let destination = root.join(&name);
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_symlink() {
                symlink(fs::read_link(entry.path())?, destination)?;
            } else if meta.is_dir() {
                fs::create_dir(destination)?;
            } else {
                File::create(destination)?;
            }
        }
        for name in reserved {
            fs::create_dir(root.join(name))?;
        }
        mount(Some(root), root, None, libc::MS_BIND, None)?;
        let mut mounts = vec![];
        for entry in fs::read_dir(toolroot)? {
            let entry = entry?;
            let name = entry.file_name();
            if reserved.iter().any(|r| name == *r) || entry.file_type()?.is_symlink() {
                continue;
            }
            let target = root.join(&name);
            mount(Some(&entry.path()), &target, None, libc::MS_BIND, None)?;
            mount(
                None,
                &target,
                None,
                libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                None,
            )?;
            mounts.push(json!({"source":entry.path(),"target":format!("/{}",name.to_string_lossy()),"readonly":true}));
        }
        mount(
            Some(folder),
            &root.join("bench-trial"),
            None,
            libc::MS_BIND,
            None,
        )?;
        let input = root.join("bench-trial/work/input");
        mount(
            Some(&toolroot.join("fs-v2-fixture")),
            &input,
            None,
            libc::MS_BIND,
            None,
        )?;
        mount(
            None,
            &input,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            None,
        )?;
        mount(
            Some(Path::new("tmpfs")),
            &root.join("tmp"),
            Some("tmpfs"),
            0,
            Some("mode=1777,size=256m"),
        )?;
        mount(
            Some(Path::new("tmpfs")),
            &root.join("run"),
            Some("tmpfs"),
            0,
            Some("mode=0755,size=16m"),
        )?;
        mount(
            Some(Path::new("proc")),
            &root.join("proc"),
            Some("proc"),
            0,
            None,
        )?;
        for name in ["dev", "sys"] {
            mount(
                Some(&PathBuf::from("/").join(name)),
                &root.join(name),
                None,
                libc::MS_BIND | libc::MS_REC,
                None,
            )?;
        }
        std::env::set_current_dir(root)?;
        let root_c = CString::new(root.as_os_str().as_bytes())?;
        let old = CString::new(root.join(".old-root").as_os_str().as_bytes())?;
        ensure!(
            unsafe { libc::syscall(libc::SYS_pivot_root, root_c.as_ptr(), old.as_ptr()) } == 0,
            "pivot_root: {}",
            std::io::Error::last_os_error()
        );
        std::env::set_current_dir("/")?;
        ensure!(
            unsafe { libc::umount2(c"/.old-root".as_ptr(), libc::MNT_DETACH) } == 0,
            "detach old root"
        );
        let receipt = json!({"schema":1,"backend":config["backend"],"namespaces":identities,
        "parent_namespaces":config["parent_namespaces"],"readonly_prepared_mounts":mounts,
        "old_root_detached":true,"root_switch":"pivot_root","mountinfo":fs::read_to_string("/proc/self/mountinfo")?});
        fs::write(
            "/bench-trial/namespace.json",
            serde_json::to_vec_pretty(&receipt)?,
        )?;
        std::env::set_current_dir("/bench-trial/work")?;
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        ensure!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } == 0,
            "clock"
        );
        println!(
            "USER_WORKLOADS_LAUNCH {}",
            json!({"monotonic_ns":now.tv_sec as u128*1_000_000_000+now.tv_nsec as u128,"backend":config["backend"]})
        );
        std::io::stdout().flush()?;
        let argv = config["argv"].as_array().context("argv")?;
        let argv: Vec<_> = argv
            .iter()
            .map(|v| v.as_str().context("argv string"))
            .collect::<Result<_>>()?;
        Err(Command::new(argv[0]).args(&argv[1..]).exec().into())
    }
    pub fn main() -> Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.first().and_then(|s| s.to_str()) == Some("launch") {
            ensure!(args.len() >= 2, "launch needs executable and argv");
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            ensure!(
                unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } == 0,
                "clock"
            );
            println!(
                "USER_WORKLOADS_LAUNCH {}",
                json!({"monotonic_ns":now.tv_sec as u128*1_000_000_000+now.tv_nsec as u128})
            );
            std::io::stdout().flush()?;
            return Err(Command::new(&args[1]).args(&args[2..]).exec().into());
        }
        ensure!(args.len() == 2, "expected MODE FIXTURE or namespace CONFIG");
        let mode = args[0].to_str().context("mode")?;
        let path = Path::new(&args[1]);
        match mode {
            "prepare" => prepare(path)?,
            "validate" => validate(path)?,
            "read" => {
                read(path)?;
                println!("{}", json!({"workload":mode,"correctness":"passed"}));
            }
            "metadata" => {
                metadata(path)?;
                println!("{}", json!({"workload":mode,"correctness":"passed"}));
            }
            #[cfg(target_os = "linux")]
            "namespace" => namespace(path)?,
            _ => bail!("unknown mode {mode}"),
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn fixture_manifest_is_fixed() {
            assert_eq!(expected()["tree_bytes"], 2048 * 22);
            assert_eq!(chunk().len(), 1 << 20);
        }
        #[test]
        fn symlink_inventory_is_rejected() {
            let temp = tempfile::tempdir().unwrap();
            symlink("/etc/passwd", temp.path().join("foreign")).unwrap();
            assert!(inventory(temp.path()).is_err());
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("filesystem-worker requires Linux namespaces");
    std::process::exit(2);
}

//! Fresh immutable assets for the nine-runtime comparison. No VM/task sampling.
use super::{
    layered::{cp, docker, string},
    support::*,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn files(root: &Path) -> Result<Value> {
    fn visit(root: &Path, path: &Path, out: &mut serde_json::Map<String, Value>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, out)?;
            } else if metadata.is_file() {
                out.insert(
                    path.strip_prefix(root)?.to_string_lossy().into_owned(),
                    json!(sha(&path)?),
                );
            }
        }
        Ok(())
    }
    let mut out = serde_json::Map::new();
    visit(root, root, &mut out)?;
    Ok(Value::Object(out))
}
fn build(source: &Path, target: &Path, folder: &Path) -> Result<()> {
    run(
        Command::new("rustc")
            .args([
                "--edition=2024",
                "-O",
                "--target",
                "x86_64-unknown-linux-musl",
            ])
            .arg(source)
            .arg("-o")
            .arg(target),
        folder,
        Duration::from_secs(600),
        &[],
    )?;
    Ok(())
}
fn fixture(layer: &Path) -> Result<PathBuf> {
    let root = layer.join("fs-v2-fixture");
    fs::create_dir_all(root.join("meta"))?;
    let mut bytes = Vec::with_capacity(4 << 20);
    for i in 0..(4 << 20) / 8 {
        let mut x = 0x4653563246495854u64.wrapping_add((i as u64).wrapping_mul(0x9e3779b97f4a7c15));
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^= x >> 31;
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    fs::write(root.join("read.bin"), &bytes)?;
    fs::write(root.join("sentinel.bin"), &bytes[..64])?;
    for i in 0..2048 {
        fs::write(root.join(format!("meta/f{i:08}.bin")), &bytes[..64])?;
    }
    // Write USTAR directly to keep archive metadata deterministic, with no
    // Python interpreter or filesystem-dependent tar ordering.
    let mut archive = File::create(root.join("package.tar"))?;
    let repo = root.join("repo");
    fs::create_dir(&repo)?;
    fn octal(field: &mut [u8], value: u64) {
        let text = format!("{value:0width$o}\0", width = field.len() - 1);
        field.copy_from_slice(text.as_bytes());
    }
    for i in 0..1024 {
        let relative = format!("d{:02}/f{:03}", i / 32, i % 32);
        let path = repo.join(format!("{relative}.txt"));
        fs::create_dir_all(path.parent().unwrap())?;
        let prefix = format!("file {i:04} V2_NEEDLE\n");
        let mut content = vec![b'x'; 4096];
        content[..prefix.len()].copy_from_slice(prefix.as_bytes());
        fs::write(path, content)?;
        let name = format!("{relative}.dat");
        let prefix = format!("package {i:04}\n");
        let mut content = vec![b'p'; 4096];
        content[..prefix.len()].copy_from_slice(prefix.as_bytes());
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        octal(&mut header[100..108], 0o644);
        octal(&mut header[108..116], 0);
        octal(&mut header[116..124], 0);
        octal(&mut header[124..136], 4096);
        octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u64 = header.iter().map(|b| *b as u64).sum();
        let sum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(sum.as_bytes());
        archive.write_all(&header)?;
        archive.write_all(&content)?;
    }
    archive.write_all(&[0; 1024])?;
    for args in [
        vec!["init", "-q", "-b", "fixture"],
        vec!["add", "."],
        vec![
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixed offline fixture",
        ],
    ] {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "filesystem-v2")
            .env("GIT_COMMITTER_NAME", "filesystem-v2")
            .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
            .env("GIT_COMMITTER_EMAIL", "fixture@localhost")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z");
        checked(&mut command)?;
    }
    let build = root.join("build");
    fs::create_dir(&build)?;
    let mut main = String::from("extern int printf(const char *, ...);\n");
    for i in 0..100 {
        fs::write(
            build.join(format!("unit{i:03}.c")),
            format!("int unit{i:03}(void) {{ return {i}; }}\n"),
        )?;
        main.push_str(&format!("int unit{i:03}(void);\n"));
    }
    main.push_str("int main(void) { int sum = 0;\n");
    for i in 0..100 {
        main.push_str(&format!("sum += unit{i:03}();\n"));
    }
    main.push_str("printf(\"%d\\n\", sum); return 0; }\n");
    fs::write(build.join("main.c"), main)?;
    Ok(root)
}
fn install(source: &Path, target: &Path, root: &Path) -> Result<Value> {
    // Prepared-root symlinks must not redirect writes to the real host.
    let parent = target.parent().context("install parent")?;
    fs::create_dir_all(parent)?;
    ensure!(
        parent.canonicalize()?.starts_with(root.canonicalize()?),
        "install escaped private root"
    );
    if fs::symlink_metadata(target).is_ok() {
        fs::remove_file(target)?;
    }
    fs::copy(source.canonicalize()?, target)?;
    Ok(json!({"path":target.strip_prefix(root)?,"source":source,"sha256":sha(target)?}))
}
fn kernel(receipt: &Path, target: &Path) -> Result<(Value, Value)> {
    let value = load(receipt)?;
    ensure!(
        value["schema"] == "pvisor-firecracker-kernel/v1"
            && value["variant"] == "fc-system"
            && value["provenance"]["origin"] == "official-distro-stock"
            && value["provenance"]["pvisor_firmware"] == false,
        "official stock kernel receipt required"
    );
    for name in ["distribution", "package", "package_version", "source_url"] {
        ensure!(
            !string(&value["provenance"], name)?.trim().is_empty(),
            "kernel provenance incomplete"
        );
    }
    let artifacts = value["artifacts"].as_object().context("kernel artifacts")?;
    ensure!(
        artifacts.len() == 6
            && [
                "kernel",
                "vmlinuz",
                "config",
                "provenance",
                "extractor",
                "initrd"
            ]
            .iter()
            .all(|n| artifacts.contains_key(*n)),
        "stock kernel inventory"
    );
    fs::create_dir(target)?;
    let mut paths = serde_json::Map::new();
    for (name, item) in artifacts {
        let relative = Path::new(string(item, "file")?);
        ensure!(
            relative.components().count() == 1,
            "kernel artifact must be direct child"
        );
        let source = receipt.parent().unwrap().join(relative);
        ensure!(
            !fs::symlink_metadata(&source)?.is_symlink()
                && sha(&source)? == string(item, "sha256")?,
            "kernel artifact changed"
        );
        fs::copy(source, target.join(relative))?;
        paths.insert(name.clone(), json!(target.join(relative)));
    }
    ensure!(
        fs::read(Path::new(paths["kernel"].as_str().unwrap()))?.starts_with(b"\x7fELF"),
        "Firecracker stock ELF"
    );
    ensure!(
        load(Path::new(paths["provenance"].as_str().unwrap()))? == value["provenance"],
        "provenance receipt bytes"
    );
    fs::copy(receipt, target.join("kernel-receipt.json"))?;
    Ok((value, Value::Object(paths)))
}
fn references(output: &Path, toolroot: &Path, config: &Value, harness: &Path) -> Result<Value> {
    let root = output.join("reference-assets/vm");
    fs::create_dir_all(&root)?;
    let (receipt, paths) = kernel(
        Path::new(string(config, "stock_kernel_receipt")?),
        &root.join("stock-kernel"),
    )?;
    let mut runtimes = serde_json::Map::new();
    fs::create_dir(root.join("bin"))?;
    for (key, default) in [
        ("firecracker", "firecracker"),
        ("qemu-system-x86_64", "qemu-system-x86_64"),
    ] {
        let source = if let Some(s) = config[key].as_str() {
            PathBuf::from(s)
        } else {
            let path = checked(Command::new("which").arg(default))?;
            PathBuf::from(path.trim())
        };
        let target = root.join("bin").join(key);
        fs::copy(&source, &target)?;
        let version = checked(Command::new(&target).arg("--version"))?;
        runtimes.insert(
            key.into(),
            json!({"source":source,"executable":target,"sha256":sha(&target)?,"version":version}),
        );
    }
    let data = root.join("qemu-data");
    fs::create_dir(&data)?;
    let mut firmware = serde_json::Map::new();
    let mut sources = vec![PathBuf::from("/usr/share/seabios/bios-256k.bin")];
    for entry in fs::read_dir("/usr/share/qemu")? {
        let path = entry?.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("bin" | "rom")
            )
        {
            sources.push(path);
        }
    }
    for source in sources {
        let target = data.join(source.file_name().unwrap());
        fs::copy(&source, &target)?;
        firmware.insert(
            source.file_name().unwrap().to_string_lossy().into_owned(),
            json!({"path":target,"source":source,"sha256":sha(&target)?}),
        );
    }
    ensure!(
        ["bios-256k.bin", "qboot.rom", "linuxboot_dma.bin"]
            .iter()
            .all(|k| firmware.contains_key(*k)),
        "QEMU firmware inventory"
    );
    build(
        &harness.join("reference_init.rs"),
        &root.join("init"),
        &root.join("init-build"),
    )?;
    let tree = root.join("image-tree");
    fs::create_dir(&tree)?;
    cp(&toolroot.join("."), &tree)?;
    fs::create_dir_all(tree.join("bench-work"))?;
    fs::create_dir_all(tree.join("bench"))?;
    install(&root.join("init"), &tree.join("bench/init"), &tree)?;
    let seed = root.join("work-seed");
    fs::create_dir(&seed)?;
    for name in ["input", "output"] {
        fs::create_dir(seed.join(name))?;
    }
    for (name, source) in [("root", &tree), ("work", &seed)] {
        let disk = root.join(format!("{name}.ext4"));
        File::create(&disk)?.set_len(8 << 30)?;
        run(
            Command::new("mke2fs")
                .args([
                    "-q",
                    "-F",
                    "-t",
                    "ext4",
                    "-b",
                    "4096",
                    "-m",
                    "0",
                    "-E",
                    "lazy_itable_init=0,lazy_journal_init=0",
                    "-d",
                ])
                .arg(source)
                .arg(&disk),
            &root.join(format!("{name}-prepare")),
            Duration::from_secs(600),
            &[],
        )?;
    }
    fs::remove_dir_all(tree)?;
    fs::remove_dir_all(seed)?;
    let assets = json!({"root_disk":root.join("root.ext4"),"work_disk":root.join("work.ext4"),
        "kernel_receipt":root.join("stock-kernel/kernel-receipt.json"),"kernel_paths":paths,
        "runtimes":runtimes,"qemu_data_dir":data,"qemu_firmware":firmware,"artifacts":files(&root)?});
    let mut result = serde_json::Map::new();
    for name in ["fc-system", "qemu", "qemu-microvm"] {
        result.insert(name.into(),json!({"guest_vcpus":2,"guest_memory_mib":512,"assets":assets,
            "kernel":receipt["provenance"],"storage":"private ext4 root/work disks, virtio-blk, explicit Writeback with flush"}));
    }
    Ok(Value::Object(result))
}
fn docker_assets(
    output: &Path,
    toolroot: &Path,
    setup_path: &Path,
    harness: &Path,
) -> Result<Value> {
    let setup = load(setup_path)?;
    ensure!(
        setup["state"] == "ready-for-image-import-and-independent-runtime-capability-probes"
            && setup["memory_bytes"] == 2u64 << 30
            && setup["swap_bytes"] == 0
            && setup["cpu_ids"] == json!([0, 1])
            && setup["cpu_quota"] == 2
            && setup["docker_info"]["Driver"] == "overlay2"
            && setup["docker_info"]["CgroupDriver"] == "systemd",
        "new private rootless Docker budget setup required"
    );
    let config = json!({"docker_binary":setup["tools"]["docker"],"docker_host":setup["docker_host"],
        "budget_slice_unit":setup["slice_unit"],"budget_cgroup":setup["cgroup"],"setup_receipt":setup_path,
        "setup_sha256":sha(setup_path)?,"storage_driver":"overlay2","cpu_placement":setup["cpu_placement"]});
    budget(Some(Path::new(string(&config, "budget_cgroup")?)), false)?;
    // Revalidate the live daemon instead of trusting only a stale receipt.
    let info: Value = serde_json::from_str(&checked(docker(&config)?.args([
        "info",
        "--format",
        "{{json .}}",
    ]))?)?;
    ensure!(
        info["Driver"] == "overlay2"
            && info["CgroupDriver"] == "systemd"
            && info["SecurityOptions"]
                .as_array()
                .context("Docker security options")?
                .iter()
                .any(|v| v.as_str().unwrap_or("").contains("rootless")),
        "daemon changed or not rootless"
    );
    let assets = output.join("docker-assets");
    fs::create_dir(&assets)?;
    build(
        &harness.join("user_workloads_docker_launch.rs"),
        &assets.join("docker-runtime"),
        &assets.join("compile"),
    )?;
    let tree = assets.join("image-tree");
    fs::create_dir(&tree)?;
    cp(&toolroot.join("."), &tree)?;
    install(
        &assets.join("docker-runtime"),
        &tree.join("bench-artifacts/docker-runtime"),
        &tree,
    )?;
    for name in ["bench-trial/work/input", "bench-trial/work/output"] {
        fs::create_dir_all(tree.join(name))?;
    }
    let archive = assets.join("image.tar");
    run(
        Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .arg("."),
        &assets.join("archive"),
        Duration::from_secs(600),
        &[],
    )?;
    let (_, text) = run(
        docker(&config)?
            .args(["import", "--change", "WORKDIR /bench-trial/work"])
            .arg(&archive)
            .arg(format!("pvisor-fs-user-rust:{}", std::process::id())),
        &assets.join("import"),
        Duration::from_secs(600),
        &[],
    )?;
    let image = text.trim();
    ensure!(
        image.starts_with("sha256:") && image.len() == 71,
        "Docker immutable image ID"
    );
    fs::remove_file(archive)?;
    fs::remove_dir_all(tree)?;
    let inspection: Value = serde_json::from_str(&checked(
        docker(&config)?.args(["image", "inspect", image]),
    )?)?;
    save(assets.join("image-inspect.json"), &inspection)?;
    let mut config = config;
    config["image_id"] = json!(image);
    config["runtime_sha256"] = json!(sha(&assets.join("docker-runtime"))?);
    save(assets.join("receipt.json"), &config)?;
    Ok(config)
}
pub fn prepare(output: &Path) -> Result<()> {
    let config = load(required("PVISOR_BENCH_LAYERED_CONFIG")?)?;
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let harness = output.join("harness");
    fs::create_dir(&harness)?;
    let source = repo.join("benchmark/filesystem");
    for name in [
        "workloads_v2_guest.rs",
        "workloads_v2_scenarios.rs",
        "user_workloads_cwd.rs",
        "user_workloads_docker_launch.rs",
        "reference_init.rs",
    ] {
        fs::copy(source.join(name), harness.join(name))?;
    }
    let bin = output.join("bin");
    fs::create_dir(&bin)?;
    build(
        &harness.join("workloads_v2_guest.rs"),
        &bin.join("payload"),
        &output.join("payload-build"),
    )?;
    let layer = output.join("fixture-layer");
    let fixture = fixture(&layer)?;
    save(output.join("fixture-manifest.json"), &files(&layer)?)?;
    let toolroot = output.join("toolroot");
    fs::create_dir(&toolroot)?;
    cp(&Path::new(string(&config, "rootfs")?).join("."), &toolroot)?;
    cp(&layer.join("."), &toolroot)?;
    fs::create_dir_all(toolroot.join("bench-artifacts"))?;
    let mut installed = vec![install(
        &bin.join("payload"),
        &toolroot.join("bench-artifacts/workload"),
        &toolroot,
    )?];
    build(
        &harness.join("user_workloads_cwd.rs"),
        &toolroot.join("bench-artifacts/payload"),
        &output.join("wrapper-build"),
    )?;
    installed.push(json!({"path":"bench-artifacts/payload","sha256":sha(&toolroot.join("bench-artifacts/payload"))?}));
    let artifacts = Path::new(string(&config, "artifacts")?);
    fs::copy(
        artifacts.join("build-receipt.json"),
        output.join("cli-build-receipt.json"),
    )?;
    for name in ["pvisor", "pvisor-guest"] {
        installed.push(install(
            &artifacts.join("bin").join(name),
            &toolroot.join("bench-artifacts").join(name),
            &toolroot,
        )?);
    }
    let firmware = Path::new(string(&config, "firmware")?);
    let firmware_source = Path::new(string(&config, "firmware_source")?);
    ensure!(
        load(firmware_source)?["firmware_sha256"] == sha(firmware)?,
        "firmware SOURCE mismatch"
    );
    fs::copy(firmware, bin.join("libkrunfw.so"))?;
    fs::copy(firmware_source, output.join("firmware-source.json"))?;
    installed.push(install(
        firmware,
        &toolroot.join("bench-artifacts/libkrunfw.so.5.6.2"),
        &toolroot,
    )?);
    for name in ["libkrunfw.so", "libkrunfw.so.5"] {
        let target = toolroot.join("bench-artifacts").join(name);
        if fs::symlink_metadata(&target).is_ok() {
            fs::remove_file(&target)?;
        }
        std::os::unix::fs::symlink("libkrunfw.so.5.6.2", target)?;
    }
    // Freeze the actual shared-library inputs. Static product binaries are
    // accepted; toolchain binaries still require all their runtime libraries.
    let mut dependencies = vec![];
    for (index, executable) in [
        "bench-artifacts/pvisor",
        "bench-artifacts/pvisor-guest",
        "usr/bin/cc",
        "usr/bin/as",
        "usr/bin/ld",
        "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
        "usr/libexec/gcc/x86_64-redhat-linux/16/collect2",
    ]
    .into_iter()
    .enumerate()
    {
        let path = toolroot.join(executable);
        ensure!(path.is_file(), "missing prepared tool {executable}");
        let logs = output.join(format!("ldd-{index}"));
        let status = run(
            Command::new("ldd").arg(&path),
            &logs,
            Duration::from_secs(30),
            &[],
        );
        let text = format!(
            "{}{}",
            fs::read_to_string(logs.join("stdout.log"))?,
            fs::read_to_string(logs.join("stderr.log"))?
        );
        ensure!(
            status.is_ok()
                || text.contains("not a dynamic executable")
                || text.contains("statically linked"),
            "ldd failed {executable}"
        );
        ensure!(!text.contains("not found"), "unresolved runtime dependency");
        for line in text.lines() {
            let path = line.split_whitespace().find(|s| s.starts_with('/'));
            if let Some(path) = path {
                let source = Path::new(path);
                let target = toolroot.join(source.strip_prefix("/")?);
                if !target.is_file() {
                    dependencies.push(install(source, &target, &toolroot)?);
                }
            }
        }
    }
    for relative in ["usr/lib64/libatomic.so.1.2.0", "usr/lib64/libatomic.so.1"] {
        if !toolroot.join(relative).is_file() {
            dependencies.push(install(
                &Path::new("/").join(relative),
                &toolroot.join(relative),
                &toolroot,
            )?);
        }
    }
    save(
        output.join("toolroot-receipt.json"),
        &json!({"installed":installed,"runtime_dependencies":dependencies,"toolroot":toolroot}),
    )?;
    let mut refs = references(output, &toolroot, &config, &harness)?;
    refs["docker"] = docker_assets(
        output,
        &toolroot,
        Path::new(string(&config, "docker_setup")?),
        &harness,
    )?;
    let cases: Vec<_> = (1..=12)
        .map(|i| format!("M{i:02}"))
        .chain((1..=4).map(|i| format!("R{i:02}")))
        .chain((1..=3).map(|i| format!("S{i:02}")))
        .collect();
    let mut tools = serde_json::Map::new();
    for name in [
        "usr/bin/cc",
        "usr/bin/git",
        "usr/bin/python3",
        "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
    ] {
        tools.insert(name.into(), json!(sha(&toolroot.join(name))?));
    }
    let mut artifact_hashes = serde_json::Map::new();
    for dir in ["harness", "bin", "reference-assets", "docker-assets"] {
        for (name, value) in files(&output.join(dir))?.as_object().unwrap() {
            artifact_hashes.insert(format!("{dir}/{name}"), value.clone());
        }
    }
    for name in [
        "toolroot-receipt.json",
        "cli-build-receipt.json",
        "firmware-source.json",
    ] {
        artifact_hashes.insert(name.into(), json!(sha(&output.join(name))?));
    }
    let protocol = json!({"schema":2,"benchmark_id":"B-FS-TOOLS","role":"user-facing","rounds":3,"ram_mib":512,
        "samples":"Criterion fresh randomized cohorts, not old N3 statistics","cases":cases,"trial_timeout_seconds":90,
        "backends":(super::layered::MODES.map(|id|json!({"id":id}))),
        "toolroot":toolroot,"fixture":fixture,"prepared_tools":tools,"reference_configuration":refs,
        "fixture_manifest_sha256":sha(&output.join("fixture-manifest.json"))?,"artifacts":artifact_hashes,
        "quiet_gate":false,"interference_gate":false,"skip_remaining_on_case_failure":true,
        "budget":{"cpus":[0,1],"quota":"200000 100000","memory_bytes":2u64<<30,"swap_bytes":0,"guest_mib":512,"guest_vcpus":2}});
    save(output.join("protocol.json"), &protocol)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern_first_word_matches_payload() {
        let mut x = 0x4653563246495854u64;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^= x >> 31;
        assert_ne!(x, 0);
    }
    #[test]
    fn generated_fixture_has_exact_offline_workflows() {
        let temp = tempfile::tempdir().unwrap();
        let root = fixture(&temp.path().join("fixture-layer")).unwrap();
        assert_eq!(fs::read_dir(root.join("meta")).unwrap().count(), 2048);
        assert_eq!(fs::metadata(root.join("read.bin")).unwrap().len(), 4 << 20);
        assert_eq!(
            fs::read(root.join("sentinel.bin")).unwrap(),
            fs::read(root.join("read.bin")).unwrap()[..64]
        );
        assert_eq!(
            fs::read_to_string(root.join("build/unit099.c")).unwrap(),
            "int unit099(void) { return 99; }\n"
        );
        let listing =
            checked(Command::new("tar").arg("-tf").arg(root.join("package.tar"))).unwrap();
        assert_eq!(listing.lines().count(), 1024);
        assert_eq!(listing.lines().next(), Some("d00/f000.dat"));
        assert_eq!(listing.lines().last(), Some("d31/f031.dat"));
        let content = checked(
            Command::new("tar")
                .arg("-xOf")
                .arg(root.join("package.tar"))
                .arg("d31/f031.dat"),
        )
        .unwrap();
        assert_eq!(content.len(), 4096);
        assert!(content.starts_with("package 1023\n"));
        let status = checked(
            Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(root.join("repo")),
        )
        .unwrap();
        assert!(status.is_empty());
    }
}

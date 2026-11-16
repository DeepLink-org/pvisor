//! Native input preparation for active MEMORY experiments. Never samples.
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[path = "../criterion/src/memory/fixture.rs"]
mod fixture;
#[path = "../criterion/src/memory/process.rs"]
mod process;
use process::OwnedChild as Process;

#[derive(Parser)]
#[command(about = "Freeze native MEMORY inputs; no sampling or benchmark statistics")]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Build/freeze native VM, static and optional dlopen fixtures and images.
    Pool {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long, default_value = "/usr/bin/firecracker")]
        firecracker: PathBuf,
        #[arg(long)]
        firecracker_filter: Option<PathBuf>,
        #[arg(long)]
        firecracker_build_receipt: Option<PathBuf>,
        #[arg(long)]
        library_rootfs: Option<PathBuf>,
    },
    /// Build/freeze the SDK scanner probe and clone its private rootfs.
    Scanner {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        firmware: PathBuf,
    },
    /// Freeze explicitly built/signed allocator artifacts and a private rootfs.
    Allocator {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        vmm: PathBuf,
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        so: PathBuf,
        #[arg(long)]
        build_receipt: PathBuf,
    },
    /// Import frozen native fixtures into a NEW private rootless Docker seed.
    Docker {
        #[arg(long)]
        inputs: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    #[command(hide = true)]
    DockerImport { root: PathBuf, slice: String },
    #[command(hide = true)]
    Lease { unit: String },
}

fn repo() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}
fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("hash {}", path.display()))?;
    let mut sha = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        sha.update(&block[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}
/// Every external tool retains argv, working directory, stdout, stderr and exit.
fn run(root: &Path, label: &str, cwd: &Path, args: &[OsString], timeout: u64) -> Result<Vec<u8>> {
    ensure!(!args.is_empty(), "empty preparation command");
    save(
        &root.join(format!("{label}.command.json")),
        &json!({
            "argv":args.iter().map(|s|s.to_string_lossy()).collect::<Vec<_>>(),
            "cwd":cwd,"timeout_seconds":timeout,"state":"started"
        }),
    )?;
    let mut child = Process::spawn(
        Command::new(&args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(File::create(root.join(format!("{label}.stdout")))?)
            .stderr(File::create(root.join(format!("{label}.stderr")))?),
    )?;
    let status = child.wait(timeout);
    save(
        &root.join(format!("{label}.command.json")),
        &json!({
            "argv":args.iter().map(|s|s.to_string_lossy()).collect::<Vec<_>>(),"cwd":cwd,
            "timeout_seconds":timeout,"status":status.as_ref().ok().map(ToString::to_string),
            "error":status.as_ref().err().map(ToString::to_string)
        }),
    )?;
    ensure!(
        status?.success(),
        "preparation tool failed: {label}; logs retained"
    );
    Ok(fs::read(root.join(format!("{label}.stdout")))?)
}
fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}
fn fresh(path: &Path) -> Result<PathBuf> {
    fs::create_dir(path).context("preparation output must be NEW")?;
    let root = path.canonicalize()?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    Ok(root)
}
fn copy_tree(root: &Path, source: &Path, destination: &Path, label: &str) -> Result<()> {
    ensure!(
        source.is_dir() && !source.is_symlink(),
        "rootfs must be a real directory"
    );
    ensure!(
        !destination.starts_with(source.canonicalize()?),
        "fixture destination must not be inside its source tree"
    );
    run(
        root,
        label,
        root,
        &[
            "cp".into(),
            "-a".into(),
            source.as_os_str().into(),
            destination.as_os_str().into(),
        ],
        120,
    )?;
    Ok(())
}
fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    ensure!(
        source.is_file(),
        "input is not a file: {}",
        source.display()
    );
    fs::copy(source, destination)?;
    fs::set_permissions(destination, source.metadata()?.permissions())?;
    Ok(())
}
/// Same file/link/mode DTO as historical receipts; directories are never followed.
fn tree_manifest(root: &Path) -> Result<Value> {
    fn walk(root: &Path, path: &Path, rows: &mut Vec<Value>) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        let relative = path.strip_prefix(root)?;
        let name = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let mut row = json!({"path":name,"mode":metadata.mode()&0o7777});
        if metadata.file_type().is_symlink() {
            row["symlink"] = json!(fs::read_link(path)?);
        } else if metadata.is_file() {
            row["bytes"] = json!(metadata.len());
            row["sha256"] = json!(hash(path)?);
        } else if metadata.is_dir() {
            row["directory"] = json!(true);
        } else {
            bail!("unsupported fixture object {}", path.display())
        }
        rows.push(row);
        if metadata.is_dir() {
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                walk(root, &entry.path(), rows)?;
            }
        }
        Ok(())
    }
    ensure!(
        root.is_dir() && !root.is_symlink(),
        "manifest root must be a real directory"
    );
    let mut rows = Vec::new();
    walk(root, root, &mut rows)?;
    rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!(rows))
}
fn source_inputs(repository: &Path, names: &[u8]) -> Result<Value> {
    let mut rows = Vec::new();
    let names = names
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| std::str::from_utf8(b).map(str::to_owned))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    for name in names {
        let selected = [
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config",
            ".cargo/config.toml",
        ]
        .contains(&name.as_str())
            || ["crates/", "vendor/", "fw/", "benchmark/criterion/"]
                .iter()
                .any(|p| name.starts_with(p))
            || (name.starts_with("benchmark/memory/")
                && (name.ends_with(".rs") || name.ends_with(".c")));
        if !selected {
            continue;
        }
        let path = repository.join(&name);
        let mut row = json!({"path":name});
        if path.is_symlink() {
            row["symlink"] = json!(fs::read_link(&path)?);
        } else if path.is_file() {
            row["bytes"] = json!(path.metadata()?.len());
            row["sha256"] = json!(hash(&path)?);
        } else {
            row["absent"] = json!(true);
        }
        rows.push(row);
    }
    Ok(json!(rows))
}
fn source_snapshot(root: &Path, repository: &Path) -> Result<Value> {
    let list = run(
        root,
        "source-list",
        repository,
        &words(&[
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]),
        30,
    )?;
    let manifest = source_inputs(repository, &list)?;
    save(&root.join("source-files.json"), &manifest)?;
    let mut names = File::create(root.join("archive-files.list"))?;
    for row in manifest
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["absent"] != true)
    {
        names.write_all(row["path"].as_str().unwrap().as_bytes())?;
        names.write_all(&[0])?;
    }
    names.sync_all()?;
    run(
        root,
        "source-archive",
        repository,
        &[
            "tar".into(),
            "--null".into(),
            "--no-recursion".into(),
            "-czf".into(),
            root.join("source.tar.gz").into_os_string(),
            "-T".into(),
            root.join("archive-files.list").into_os_string(),
        ],
        120,
    )?;
    let patch = run(
        root,
        "git-patch",
        repository,
        &words(&["git", "diff", "--binary", "HEAD"]),
        30,
    )?;
    fs::write(root.join("source.patch"), patch)?;
    let status = run(
        root,
        "git-status",
        repository,
        &words(&["git", "status", "--porcelain=v1", "--untracked-files=all"]),
        30,
    )?;
    fs::write(root.join("source.status"), status)?;
    Ok(manifest)
}
fn unchanged_sources(root: &Path, repository: &Path, before: &Value) -> Result<()> {
    let list = run(
        root,
        "source-list-after",
        repository,
        &words(&[
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]),
        30,
    )?;
    ensure!(
        source_inputs(repository, &list)? == *before,
        "source changed during preparation build"
    );
    Ok(())
}
fn provenance(root: &Path, repository: &Path, runtime: &[&str]) -> Result<Value> {
    let mut hashes = serde_json::Map::new();
    for name in runtime {
        hashes.insert((*name).into(), json!(hash(&root.join(name))?));
    }
    let head = run(
        root,
        "git-head",
        repository,
        &words(&["git", "rev-parse", "HEAD"]),
        10,
    )?;
    let compiler = run(root, "compiler", repository, &words(&["rustc", "-Vv"]), 10)?;
    Ok(json!({
        "hashes":hashes,"source_head":String::from_utf8(head)?.trim(),
        "source_dirty":!fs::read(root.join("source.status"))?.is_empty(),
        "source_archive_sha256":hash(&root.join("source.tar.gz"))?,
        "source_manifest_sha256":hash(&root.join("source-files.json"))?,
        "compiler":String::from_utf8(compiler)?,
        "preparation_executable_sha256":hash(&std::env::current_exe()?)?,
        "dependency_scope":"external Cargo registry cache and system tool sources are not archived; exact input/generated bytes and build commands retained"
    }))
}
fn image(root: &Path, tree: &Path, name: &str, bytes: u64) -> Result<()> {
    let path = root.join(name);
    File::options()
        .write(true)
        .create_new(true)
        .open(&path)?
        .set_len(bytes)?;
    run(
        root,
        &name.replace('.', "-"),
        root,
        &[
            "mkfs.ext4".into(),
            "-q".into(),
            "-F".into(),
            "-d".into(),
            tree.as_os_str().into(),
            path.into_os_string(),
        ],
        120,
    )?;
    Ok(())
}
fn pool(
    output: PathBuf,
    firmware: PathBuf,
    kernel: PathBuf,
    firecracker: PathBuf,
    filter: Option<PathBuf>,
    fc_receipt: Option<PathBuf>,
    library_rootfs: Option<PathBuf>,
) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "pool preparation requires Linux x86_64"
    );
    let repository = repo()?;
    let root = fresh(&output)?;
    let result = (|| {
        let before = source_snapshot(&root, &repository)?;
        run(
            &root,
            "product-build",
            &repository,
            &words(&[
                "cargo",
                "build",
                "--locked",
                "--offline",
                "--release",
                "-p",
                "pvisor-daemon",
                "--bin",
                "pvisor-daemon",
            ]),
            900,
        )?;
        let manifest = repository.join("benchmark/criterion/Cargo.toml");
        let target = repository.join("target/criterion");
        let mut native = words(&[
            "cargo",
            "build",
            "--locked",
            "--offline",
            "--release",
            "--manifest-path",
        ]);
        native.extend([
            manifest.clone().into_os_string(),
            "--target-dir".into(),
            target.clone().into_os_string(),
        ]);
        native.extend(words(&[
            "--features",
            "vm-probes",
            "--bin",
            "pvisor-memory-vm",
            "--bin",
            "pvisor-memory-guest",
        ]));
        run(&root, "native-build", &repository, &native, 900)?;
        // Static oracle is built from the same Rust source without dynamic-loader
        // requirements; actual dlopen uses the GNU/glibc build above.
        let mut static_build = words(&[
            "cargo",
            "build",
            "--locked",
            "--offline",
            "--release",
            "--manifest-path",
        ]);
        static_build.extend([
            manifest.into_os_string(),
            "--target-dir".into(),
            target.clone().into_os_string(),
        ]);
        static_build.extend(words(&[
            "--no-default-features",
            "--target",
            "x86_64-unknown-linux-musl",
            "--bin",
            "pvisor-memory-guest",
        ]));
        run(&root, "static-build", &repository, &static_build, 900)?;
        unchanged_sources(&root, &repository, &before)?;
        for (source, name) in [
            (target.join("release/pvisor-memory-vm"), "probe"),
            (
                target.join("x86_64-unknown-linux-musl/release/pvisor-memory-guest"),
                "worker",
            ),
            (repository.join("target/release/pvisor-daemon"), "daemon"),
            (firmware, "firmware"),
            (kernel, "kernel"),
            (firecracker, "firecracker"),
        ] {
            copy_file(&source, &root.join(name))?;
        }
        let mut runtime = vec![
            "probe",
            "worker",
            "daemon",
            "firmware",
            "kernel",
            "firecracker",
            "root/worker",
            "guest.ext4",
        ];
        if let Some(filter) = filter {
            copy_file(&filter, &root.join("firecracker-filter.bpf"))?;
            runtime.push("firecracker-filter.bpf");
        }
        if let Some(receipt) = fc_receipt {
            copy_file(&receipt, &root.join("firecracker-build-receipt.json"))?;
            let receipt = read(&root.join("firecracker-build-receipt.json"))?;
            for name in ["firecracker", "firecracker-filter.bpf"] {
                if root.join(name).exists() {
                    ensure!(
                        receipt["hashes"][name] == hash(&root.join(name))?,
                        "Firecracker build receipt mismatch"
                    );
                }
            }
            runtime.push("firecracker-build-receipt.json");
        }
        let guest = root.join("root");
        fs::create_dir(&guest)?;
        for name in ["dev", "proc", "sys", "tmp", "run", "etc"] {
            fs::create_dir(guest.join(name))?;
        }
        copy_file(&root.join("worker"), &guest.join("worker"))?;
        image(&root, &guest, "guest.ext4", 96 << 20)?;
        if let Some(source) = library_rootfs {
            let tree = root.join("python-root");
            copy_tree(&root, &source.canonicalize()?, &tree, "clone-userspace")?;
            ensure!(
                !tree.join("library-worker").exists() && !tree.join("libraries").exists(),
                "reserved native fixture paths already exist"
            );
            copy_file(
                &target.join("release/pvisor-memory-guest"),
                &root.join("library-worker"),
            )?;
            copy_file(&root.join("library-worker"), &tree.join("library-worker"))?;
            let libraries = root.join("libraries");
            fs::create_dir(&libraries)?;
            copy_file(
                &repository.join("benchmark/memory/library_payload.c"),
                &root.join("library_payload.c"),
            )?;
            let mut payload = File::options()
                .write(true)
                .create_new(true)
                .open(root.join("payload"))?;
            for index in 0..fixture::SIZE / 4096 {
                payload.write_all(&fixture::page("random-shared", 0, index, 0))?;
            }
            payload.sync_all()?;
            run(
                &root,
                "library-object",
                &root,
                &words(&[
                    "objcopy",
                    "--input-target",
                    "binary",
                    "--output-target",
                    "elf64-x86-64",
                    "--binary-architecture",
                    "i386:x86-64",
                    "--rename-section",
                    ".data=.rodata,alloc,load,readonly,data,contents",
                    "payload",
                    "payload.o",
                ]),
                60,
            )?;
            run(
                &root,
                "library-alignment",
                &root,
                &words(&[
                    "objcopy",
                    "--set-section-alignment",
                    ".rodata=4096",
                    "payload.o",
                    "payload-aligned.o",
                ]),
                60,
            )?;
            run(
                &root,
                "library-link",
                &root,
                &words(&[
                    "gcc",
                    "-shared",
                    "-fPIC",
                    "-O2",
                    "-Wl,-z,noexecstack",
                    "library_payload.c",
                    "payload-aligned.o",
                    "-o",
                    "libraries/libpayload.so",
                ]),
                60,
            )?;
            fs::create_dir(tree.join("libraries"))?;
            copy_file(
                &libraries.join("libpayload.so"),
                &tree.join("libraries/libpayload.so"),
            )?;
            fs::set_permissions(
                libraries.join("libpayload.so"),
                fs::Permissions::from_mode(0o444),
            )?;
            fs::set_permissions(&libraries, fs::Permissions::from_mode(0o555))?;
            let manifest = tree_manifest(&tree)?;
            save(&root.join("python-root-manifest.json"), &manifest)?;
            let bytes = manifest
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["bytes"].as_u64().unwrap_or(0))
                .sum::<u64>();
            let quantum = 64u64 << 20;
            image(
                &root,
                &tree,
                "library.ext4",
                (3 * quantum).max((bytes * 3 / 2 + quantum).div_ceil(quantum) * quantum),
            )?;
            runtime.extend([
                "library-worker",
                "python-root/library-worker",
                "library_payload.c",
                "libraries/libpayload.so",
                "payload",
                "python-root-manifest.json",
                "library.ext4",
            ]);
        }
        let mut receipt = provenance(&root, &repository, &runtime)?;
        receipt["worker_protocol"] = json!(if root.join("library.ext4").exists() {
            "native-library-v1"
        } else {
            "native-static-v1"
        });
        receipt["binary_source_relationship"] = json!(
            "Rust-owned preparation: exact source checked unchanged across recorded Cargo builds"
        );
        receipt["fc_version"] = json!(String::from_utf8(run(
            &root,
            "firecracker-version",
            &root,
            &[
                root.join("firecracker").into_os_string(),
                "--version".into()
            ],
            10
        )?)?);
        for (label, args) in [
            ("gcc-version", vec!["gcc", "--version"]),
            ("objcopy-version", vec!["objcopy", "--version"]),
            ("mkfs-version", vec!["mkfs.ext4", "-V"]),
        ] {
            run(&root, label, &root, &words(&args), 10)?;
        }
        save(&root.join("receipt.json"), &receipt)?;
        Ok(())
    })();
    finish(&root, result)
}
fn finish(root: &Path, result: Result<()>) -> Result<()> {
    if let Err(ref error) = result {
        save(
            &root.join("failure.json"),
            &json!({"error":format!("{error:#}"),"sampling":false}),
        )?;
    } else {
        save(
            &root.join("prepared.json"),
            &json!({"complete":true,"sampling":false}),
        )?;
        println!("{}", root.display());
    }
    result
}
fn scanner(output: PathBuf, rootfs: PathBuf, firmware: PathBuf) -> Result<()> {
    let repository = repo()?;
    let root = fresh(&output)?;
    let result = (|| {
        let before = source_snapshot(&root, &repository)?;
        run(
            &root,
            "scanner-build",
            &repository,
            &words(&[
                "cargo",
                "build",
                "--locked",
                "--offline",
                "--release",
                "-p",
                "pvisor",
                "--example",
                "vm_memory_scale",
                "-p",
                "pvisor-daemon",
                "--bin",
                "pvisor-daemon",
            ]),
            900,
        )?;
        unchanged_sources(&root, &repository, &before)?;
        for (source, name) in [
            (
                repository.join("target/release/examples/vm_memory_scale"),
                "probe",
            ),
            (
                repository.join("target/release/pvisor-daemon"),
                "pool-daemon",
            ),
            (firmware, "firmware"),
        ] {
            copy_file(&source, &root.join(name))?;
        }
        copy_tree(
            &root,
            &rootfs.canonicalize()?,
            &root.join("rootfs"),
            "clone-rootfs",
        )?;
        let mut receipt = provenance(&root, &repository, &["probe", "pool-daemon", "firmware"])?;
        receipt["example_sha256"] = json!(hash(&root.join("probe"))?);
        receipt["binary_sha256"] = json!(hash(&root.join("pool-daemon"))?);
        receipt["rootfs_manifest"] = tree_manifest(&root.join("rootfs"))?;
        save(&root.join("build-receipt.json"), &receipt)?;
        Ok(())
    })();
    finish(&root, result)
}
fn allocator(
    output: PathBuf,
    source: PathBuf,
    vmm: PathBuf,
    firmware: PathBuf,
    payload: PathBuf,
    so: PathBuf,
    build_receipt: PathBuf,
) -> Result<()> {
    let repository = repo()?;
    let root = fresh(&output)?;
    let result = (|| {
        source_snapshot(&root, &repository)?;
        let supplied = read(&build_receipt)?;
        for (path, name) in [
            (vmm, "vmm"),
            (firmware, "firmware"),
            (payload, "payload"),
            (so, "reclaim.so"),
        ] {
            ensure!(
                supplied["hashes"][name] == hash(&path)?,
                "externally built allocator receipt mismatch: {name}"
            );
            copy_file(&path, &root.join(name))?;
        }
        ensure!(
            supplied["compiler"].is_string()
                && supplied["commands"].is_array()
                && supplied["source_archive_sha256"].is_string(),
            "allocator build provenance incomplete"
        );
        copy_file(&build_receipt, &root.join("supplied-build-receipt.json"))?;
        copy_tree(
            &root,
            &source.canonicalize()?,
            &root.join("root"),
            "clone-rootfs",
        )?;
        let mut receipt = provenance(
            &root,
            &repository,
            &[
                "vmm",
                "firmware",
                "payload",
                "reclaim.so",
                "supplied-build-receipt.json",
            ],
        )?;
        receipt["binary_source_relationship"] = json!(
            "caller-supplied built/signed binaries; imported build receipt, not claimed rebuilt from this checkout"
        );
        receipt["rootfs_manifest"] = tree_manifest(&root.join("root"))?;
        save(&root.join("receipt.json"), &receipt)?;
        Ok(())
    })();
    finish(&root, result)
}

fn verify_inputs(inputs: &Path) -> Result<Value> {
    let receipt = read(&inputs.join("receipt.json"))?;
    let hashes = receipt["hashes"]
        .as_object()
        .context("input hashes missing")?;
    ensure!(!hashes.is_empty(), "empty input receipt");
    for (name, wanted) in hashes {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute()
                && !path
                    .components()
                    .any(|p| p == std::path::Component::ParentDir),
            "unsafe input path"
        );
        ensure!(
            hash(&inputs.join(path))? == wanted.as_str().context("invalid input hash")?,
            "frozen input changed: {name}"
        );
    }
    ensure!(
        receipt["worker_protocol"] == "native-library-v1",
        "Docker preparation requires complete native userspace/library inputs"
    );
    ensure!(
        tree_manifest(&inputs.join("python-root"))?
            == read(&inputs.join("python-root-manifest.json"))?,
        "userspace manifest changed"
    );
    Ok(receipt)
}
struct Unit(String);
impl Drop for Unit {
    fn drop(&mut self) {
        if let Ok(mut child) = Process::spawn(
            Command::new("systemctl")
                .args(["--user", "stop", &self.0])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        ) {
            let _ = child.wait(20);
        }
    }
}
fn docker(inputs: PathBuf, output: PathBuf) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "private rootless Docker preparation requires Linux"
    );
    let inputs = inputs.canonicalize()?;
    let pool = verify_inputs(&inputs)?;
    let root = fresh(&output)?;
    ensure!(
        root.as_os_str().len() < 50,
        "short Docker preparation output required"
    );
    let result = (|| {
        let repository = repo()?;
        copy_file(
            &repository.join("benchmark/memory/docker_init.rs"),
            &root.join("docker_init.rs"),
        )?;
        run(
            &root,
            "init-build",
            &repository,
            &[
                "rustc".into(),
                "--edition=2021".into(),
                "--target".into(),
                "x86_64-unknown-linux-musl".into(),
                "-C".into(),
                "opt-level=2".into(),
                "-o".into(),
                root.join("memory-init").into_os_string(),
                root.join("docker_init.rs").into_os_string(),
            ],
            60,
        )?;
        let tree = root.join("image-root");
        copy_tree(
            &root,
            &inputs.join("python-root"),
            &tree,
            "clone-image-root",
        )?;
        for (source, name) in [
            (inputs.join("worker"), "worker"),
            (root.join("memory-init"), "memory-init"),
        ] {
            copy_file(&source, &tree.join(name))?;
        }
        run(
            &root,
            "rootfs-tar",
            &root,
            &[
                "tar".into(),
                "--numeric-owner".into(),
                "--owner=0".into(),
                "--group=0".into(),
                "-cf".into(),
                root.join("rootfs.tar").into_os_string(),
                "-C".into(),
                tree.into_os_string(),
                ".".into(),
            ],
            120,
        )?;
        let unit = format!(
            "pvisormemoryprepare{}{}.slice",
            std::process::id(),
            SystemTimeNonce::next()
        );
        let _guard = Unit(unit.clone());
        let executable = std::env::current_exe()?;
        run(
            &root,
            "slice-launch",
            &root,
            &[
                "systemd-run".into(),
                "--user".into(),
                "--quiet".into(),
                format!("--unit={}.service", unit.trim_end_matches(".slice")).into(),
                format!("--slice={unit}").into(),
                "-p".into(),
                "RuntimeMaxSec=250".into(),
                executable.clone().into_os_string(),
                "lease".into(),
                unit.clone().into(),
            ],
            15,
        )?;
        run(
            &root,
            "slice-budget",
            &root,
            &words(&[
                "systemctl",
                "--user",
                "set-property",
                "--runtime",
                &unit,
                "MemoryMax=2147483648",
                "MemorySwapMax=0",
                "CPUQuota=400%",
                "TasksMax=256",
            ]),
            15,
        )?;
        let mut args = words(&[
            "systemd-run",
            "--user",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
        ]);
        args.push(format!("--unit={}-import.service", unit.trim_end_matches(".slice")).into());
        args.push(format!("--slice={unit}").into());
        for property in [
            "Delegate=yes",
            "CPUAffinity=0 1 2 3",
            "RuntimeMaxSec=210",
            "TimeoutStopSec=15",
            "KillMode=control-group",
        ] {
            args.extend(["-p".into(), property.into()]);
        }
        args.extend([
            executable.into_os_string(),
            "docker-import".into(),
            root.clone().into_os_string(),
            unit.clone().into(),
        ]);
        let imported = run(&root, "docker-import", &root, &args, 230);
        let stopped = run(
            &root,
            "slice-stop",
            &root,
            &words(&["systemctl", "--user", "stop", &unit]),
            20,
        );
        imported?;
        stopped?;
        let state = run(
            &root,
            "slice-state",
            &root,
            &words(&[
                "systemctl",
                "--user",
                "show",
                &unit,
                "-p",
                "ActiveState",
                "-p",
                "ControlGroup",
            ]),
            5,
        )?;
        let state = String::from_utf8(state)?;
        ensure!(
            state
                .lines()
                .any(|l| l == "ActiveState=inactive" || l == "ActiveState=failed"),
            "owned Docker preparation slice remains live"
        );
        for relative in state
            .lines()
            .filter_map(|l| l.strip_prefix("ControlGroup="))
            .filter(|s| !s.is_empty())
        {
            let group = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
            ensure!(
                !group.exists()
                    || fs::read_to_string(group.join("cgroup.events"))?
                        .lines()
                        .any(|l| l == "populated 0"),
                "owned Docker preparation group still populated"
            );
        }
        save(
            &root.join("seed-manifest.json"),
            &tree_manifest(&root.join("seed"))?,
        )?;
        verify_inputs(&inputs)?;
        let mut hashes = serde_json::Map::new();
        for name in [
            "rootfs.tar",
            "image.tar",
            "memory-init",
            "docker_init.rs",
            "seed-manifest.json",
            "image-inspect.json",
        ] {
            hashes.insert(name.into(), json!(hash(&root.join(name))?));
        }
        let mut runtime = serde_json::Map::new();
        for name in [
            "docker",
            "dockerd",
            "dockerd-rootless.sh",
            "rootlesskit",
            "containerd",
            "containerd-shim-runc-v2",
            "runc",
        ] {
            let path = Path::new("/usr/bin").join(name);
            runtime.insert(path.display().to_string(), json!(hash(&path)?));
        }
        save(
            &root.join("receipt.json"),
            &json!({"schema":"pvisor-memory-docker-inputs/v1",
            "image_id":read(&root.join("image.json"))?["image_id"],"hashes":hashes,"runtime_sha256":runtime,
            "pool_receipt_sha256":hash(&inputs.join("receipt.json"))?,"source_bytes":pool["hashes"],
            "preparation_executable_sha256":hash(&std::env::current_exe()?)?,
            "scope":"same frozen native fixtures/userspace; owned rootless systemd/v2/overlayfs seed; no network; no sampling"}),
        )?;
        Ok(())
    })();
    finish(&root, result)
}
struct SystemTimeNonce;
impl SystemTimeNonce {
    fn next() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
fn docker_import(root: PathBuf, slice: String) -> Result<()> {
    let runtime = root.join("runtime");
    fs::create_dir(&runtime)?;
    let socket = runtime.join("docker.sock");
    let host = format!("--host=unix://{}", socket.display());
    let config = root.join("docker-daemon.json");
    save(&config, &json!({}))?;
    let mut daemon = Process::spawn(
        Command::new("/usr/bin/dockerd-rootless.sh")
            .arg(format!("--config-file={}", config.display()))
            .arg(format!("--data-root={}", root.join("seed").display()))
            .arg(format!("--exec-root={}", runtime.join("exec").display()))
            .arg(format!(
                "--pidfile={}",
                runtime.join("daemon.pid").display()
            ))
            .arg(&host)
            .arg(format!("--cgroup-parent={slice}"))
            .args([
                "--bridge=none",
                "--iptables=false",
                "--ip6tables=false",
                "--ip-forward=false",
                "--ip-masq=false",
                "--userland-proxy=false",
            ])
            .env(
                "DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR",
                runtime.join("kit"),
            )
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS", "false")
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_NET", "none")
            .env("DOCKERD_ROOTLESS_ROOTLESSKIT_PORT_DRIVER", "none")
            .stdin(Stdio::null())
            .stdout(File::create(root.join("dockerd.stdout"))?)
            .stderr(File::create(root.join("dockerd.stderr"))?),
    )?;
    let operation = (|| {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut attempt = 0;
        loop {
            let result = run(
                &root,
                &format!("docker-info-{attempt}"),
                &root,
                &words(&["/usr/bin/docker", &host, "info", "--format", "{{json .}}"]),
                5,
            );
            if let Ok(bytes) = result {
                let info: Value = serde_json::from_slice(&bytes)?;
                ensure!(
                    info["CgroupDriver"] == "systemd"
                        && info["CgroupVersion"] == "2"
                        && info["Driver"] == "overlayfs"
                        && info["SecurityOptions"]
                            .as_array()
                            .is_some_and(|a| a.contains(&json!("name=rootless"))),
                    "Docker rootless/systemd/v2/overlayfs capabilities differ"
                );
                save(&root.join("docker-info.json"), &info)?;
                break;
            }
            ensure!(
                daemon.running()? && Instant::now() < deadline,
                "private Docker readiness deadline"
            );
            attempt += 1;
            thread::sleep(Duration::from_millis(100));
        }
        // A filename, not stdin from a shell pipeline, imports exact retained tar.
        let args = vec![
            "/usr/bin/docker".into(),
            host.clone().into(),
            "import".into(),
            root.join("rootfs.tar").into_os_string(),
            "pvisor-memory:frozen".into(),
        ];
        let image = String::from_utf8(run(&root, "image-import", &root, &args, 120)?)?
            .trim()
            .to_string();
        ensure!(!image.is_empty(), "empty imported image identity");
        let inspection: Value = serde_json::from_slice(&run(
            &root,
            "image-inspect",
            &root,
            &words(&["/usr/bin/docker", &host, "inspect", &image]),
            15,
        )?)?;
        save(&root.join("image-inspect.json"), &inspection[0])?;
        run(
            &root,
            "image-export",
            &root,
            &[
                "/usr/bin/docker".into(),
                host.into(),
                "image".into(),
                "save".into(),
                "--output".into(),
                root.join("image.tar").into_os_string(),
                image.clone().into(),
            ],
            120,
        )?;
        save(&root.join("image.json"), &json!({"image_id":image}))?;
        Ok(())
    })();
    daemon.signal(libc::SIGTERM)?;
    let stopped = daemon.wait(15);
    save(
        &root.join("docker-cleanup.json"),
        &json!({"daemon_reaped":stopped.is_ok(),
        "errors":stopped.as_ref().err().map(ToString::to_string)}),
    )?;
    operation?;
    stopped?;
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().command {
        Action::Pool {
            output,
            firmware,
            kernel,
            firecracker,
            firecracker_filter,
            firecracker_build_receipt,
            library_rootfs,
        } => pool(
            output,
            firmware,
            kernel,
            firecracker,
            firecracker_filter,
            firecracker_build_receipt,
            library_rootfs,
        ),
        Action::Scanner {
            output,
            rootfs,
            firmware,
        } => scanner(output, rootfs, firmware),
        Action::Allocator {
            output,
            root,
            vmm,
            firmware,
            payload,
            so,
            build_receipt,
        } => allocator(output, root, vmm, firmware, payload, so, build_receipt),
        Action::Docker { inputs, output } => docker(inputs, output),
        Action::DockerImport { root, slice } => docker_import(root, slice),
        Action::Lease { unit } => {
            thread::sleep(Duration::from_secs(240));
            let status = Command::new("systemctl")
                .args(["--user", "stop", &unit])
                .status()?;
            ensure!(status.success(), "owned preparation lease stop failed");
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_preserves_modes_links_and_full_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("data"), b"fixture").unwrap();
        fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o444)).unwrap();
        std::os::unix::fs::symlink("data", root.path().join("link")).unwrap();
        let before = tree_manifest(root.path()).unwrap();
        assert_eq!(before[1]["mode"], 0o444);
        assert_eq!(before[2]["symlink"], "data");
        fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(root.path().join("data"), b"changed").unwrap();
        fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o444)).unwrap();
        assert_ne!(before, tree_manifest(root.path()).unwrap());
    }
    #[test]
    fn output_never_overwrites_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("retained"), b"evidence").unwrap();
        assert!(fresh(root.path()).is_err());
        assert_eq!(fs::read(root.path().join("retained")).unwrap(), b"evidence");
    }
    #[test]
    fn scanner_cli_preserves_literal_path_arguments() {
        let args = Args::try_parse_from([
            "pvisor-memory-prepare",
            "scanner",
            "--output",
            "/short/new output",
            "--rootfs",
            "/inputs/root$(false) space",
            "--firmware",
            "/inputs/fw space",
        ])
        .unwrap();
        let Action::Scanner {
            output,
            rootfs,
            firmware,
        } = args.command
        else {
            panic!("wrong action")
        };
        assert_eq!(output, PathBuf::from("/short/new output"));
        assert_eq!(rootfs, PathBuf::from("/inputs/root$(false) space"));
        assert_eq!(firmware, PathBuf::from("/inputs/fw space"));
    }
    #[test]
    fn preparation_tools_receive_argv_without_shell_expansion() {
        let root = tempfile::tempdir().unwrap();
        let literal = "root$(false) space";
        let out = run(
            root.path(),
            "argv",
            root.path(),
            &words(&["printf", "%s", literal]),
            5,
        )
        .unwrap();
        assert_eq!(out, literal.as_bytes());
    }
    #[test]
    fn source_selection_keeps_native_fixtures_not_python_coordinators() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("benchmark/memory")).unwrap();
        fs::write(
            root.path().join("benchmark/memory/native_worker.rs"),
            "fn main(){}",
        )
        .unwrap();
        fs::write(
            root.path().join("benchmark/memory/pool_compare.py"),
            "retired",
        )
        .unwrap();
        let rows = source_inputs(
            root.path(),
            b"benchmark/memory/native_worker.rs\0benchmark/memory/pool_compare.py\0",
        )
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["path"], "benchmark/memory/native_worker.rs");
    }
}

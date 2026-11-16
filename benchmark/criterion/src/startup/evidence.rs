use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

pub(super) fn save(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    fs::write(path.as_ref(), serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write evidence {}", path.as_ref().display()))
}

pub(super) fn read(path: impl AsRef<Path>) -> Result<Value> {
    serde_json::from_slice(
        &fs::read(path.as_ref()).with_context(|| format!("read {}", path.as_ref().display()))?,
    )
    .map_err(Into::into)
}

fn identity(info: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        info.dev(),
        info.ino(),
        info.len(),
        info.mtime(),
        info.mtime_nsec(),
        info.ctime(),
        info.ctime_nsec(),
        info.mode(),
    )
}

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let before = identity(&file.metadata()?);
    let mut hash = Sha256::new();
    let mut buffer = [0; 1024 * 64];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        identity(&file.metadata()?) == before && identity(&fs::metadata(path)?) == before,
        "artifact changed during hashing: {}",
        path.display()
    );
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    if root.is_dir() {
        for item in fs::read_dir(root)? {
            let path = item?.path();
            let info = fs::symlink_metadata(&path)?;
            if info.is_dir() && !info.is_symlink() {
                result.extend(files(&path)?);
            } else {
                result.push(path);
            }
        }
    }
    Ok(result)
}

pub(super) fn require_empty_upper(root: &Path) -> Result<()> {
    let info = fs::symlink_metadata(root).context("missing staged upper directory")?;
    ensure!(
        info.is_dir() && !info.is_symlink(),
        "staged upper must be a real directory"
    );
    ensure!(
        fs::read_dir(root)?.next().is_none(),
        "read-only workload mutated staged upper"
    );
    Ok(())
}

pub(super) fn inventory(root: &Path, include_dirs: bool) -> Result<BTreeSet<String>> {
    fn walk(base: &Path, node: &Path, dirs: bool, result: &mut BTreeSet<String>) -> Result<()> {
        let info = fs::symlink_metadata(node)?;
        if dirs || !info.is_dir() || info.is_symlink() {
            result.insert(
                node.strip_prefix(base)?
                    .to_str()
                    .context("non-UTF8 input")?
                    .to_string(),
            );
        }
        if info.is_dir() && !info.is_symlink() {
            for entry in fs::read_dir(node)? {
                walk(base, &entry?.path(), dirs, result)?;
            }
        }
        Ok(())
    }
    let mut result = BTreeSet::new();
    ensure!(
        !fs::symlink_metadata(root.join("rootfs"))?.is_symlink(),
        "rootfs must be a real directory"
    );
    walk(root, &root.join("rootfs"), include_dirs, &mut result)?;
    for name in ASSETS {
        result.insert(name.into());
    }
    Ok(result)
}

const ASSETS: [&str; 7] = [
    "vmlinux",
    "bzImage",
    "kernel.config",
    "agent-env.ext4",
    "agent-env.tar",
    "tool-identities.json",
    "assets.json",
];

/// Validate the complete frozen rootfs inventory; never follow a manifest
/// symlink while hashing it, or accept a parent that escapes the asset root.
pub(super) fn verify_inputs(root: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    let manifest = root.join("input-manifest.json");
    let raw = fs::read(&manifest)?;
    let entries: Vec<Value> = serde_json::from_slice(&raw)?;
    ensure!(!entries.is_empty(), "empty reference input manifest");
    let mut known = BTreeSet::new();
    let dirs = entries.iter().any(|entry| entry["directory"] == true);
    let mut identities = Vec::new();
    for entry in &entries {
        let relative = entry["path"]
            .as_str()
            .context("input path must be a string")?;
        let path = Path::new(relative);
        ensure!(
            !relative.is_empty()
                && !path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                && path.to_string_lossy() == relative
                && !relative.contains("//")
                && !relative.ends_with('/'),
            "input path must be canonical and relative"
        );
        ensure!(
            ASSETS.contains(&relative) || relative == "rootfs" || relative.starts_with("rootfs/"),
            "input outside declared environment"
        );
        ensure!(known.insert(relative.to_string()), "duplicate input path");
        let path = root.join(path);
        ensure!(
            path.parent()
                .context("input parent")?
                .canonicalize()?
                .starts_with(&root),
            "input parent escapes asset root"
        );
        let info = fs::symlink_metadata(&path)?;
        identities.push((path.clone(), identity(&info)));
        let object = entry.as_object().context("input entry must be an object")?;
        let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        if keys == BTreeSet::from(["path", "symlink"]) {
            ensure!(
                info.is_symlink() && fs::read_link(&path)?.to_str() == entry["symlink"].as_str(),
                "symlink differs"
            );
        } else if keys == BTreeSet::from(["path", "directory", "mode"]) {
            ensure!(
                entry["directory"] == true
                    && info.is_dir()
                    && !info.is_symlink()
                    && entry["mode"].as_u64() == Some((info.mode() & 0o7777).into()),
                "directory type/mode differs"
            );
        } else {
            ensure!(
                keys == BTreeSet::from(["path", "bytes", "sha256", "mode"])
                    || keys == BTreeSet::from(["path", "bytes", "sha256"]),
                "unknown input entry fields"
            );
            ensure!(
                info.is_file() && !info.is_symlink() && entry["bytes"].as_u64() == Some(info.len()),
                "file type/size differs: {relative}"
            );
            ensure!(
                !relative.starts_with("rootfs/") || object.contains_key("mode"),
                "rootfs input lacks mode"
            );
            if object.contains_key("mode") {
                ensure!(
                    entry["mode"].as_u64() == Some((info.mode() & 0o7777).into()),
                    "file mode differs"
                );
            }
            ensure!(
                entry["sha256"].as_str() == Some(digest(&path)?.as_str()),
                "input digest differs: {relative}"
            );
        }
    }
    for required in ASSETS.into_iter().chain([
        "rootfs/bench/reference_workload.py",
        "rootfs/bench/affinity",
    ]) {
        ensure!(
            known.contains(required),
            "reference manifest missing {required}"
        );
    }
    ensure!(
        inventory(&root, dirs)? == known,
        "reference input inventory differs"
    );
    ensure!(
        fs::read(&manifest)? == raw,
        "input manifest changed during verification"
    );
    for (path, before) in identities {
        ensure!(
            identity(&fs::symlink_metadata(path)?) == before,
            "input replaced during verification"
        );
    }
    digest(&manifest)
}

pub(super) fn verify_kernel(receipt: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let receipt = receipt.canonicalize()?;
    let record = read(&receipt)?;
    ensure!(
        record["schema"] == "pvisor-firecracker-kernel/v1" && record["variant"] == "fc-system",
        "stock kernel receipt schema/variant differs"
    );
    let provenance = &record["provenance"];
    for name in ["distribution", "package", "package_version", "source_url"] {
        ensure!(
            provenance[name]
                .as_str()
                .is_some_and(|value| !value.trim().is_empty()),
            "missing stock kernel provenance {name}"
        );
    }
    ensure!(
        provenance["origin"] == "official-distro-stock" && provenance["pvisor_firmware"] == false,
        "kernel must explicitly be stock and independent of pVisor firmware"
    );
    let artifacts = record["artifacts"]
        .as_object()
        .context("missing kernel artifacts")?;
    let required = BTreeSet::from(["kernel", "config", "provenance", "vmlinuz", "extractor"]);
    ensure!(
        required.iter().all(|name| artifacts.contains_key(*name)),
        "missing kernel artifact"
    );
    let mut paths = BTreeMap::new();
    for (name, item) in artifacts {
        ensure!(
            required.contains(name.as_str()) || name == "initrd",
            "unknown kernel artifact"
        );
        let filename = item["file"].as_str().context("kernel artifact filename")?;
        let path = receipt.parent().unwrap().join(filename);
        ensure!(
            path.parent().context("kernel parent")?.canonicalize()? == receipt.parent().unwrap()
                && !fs::symlink_metadata(&path)?.is_symlink()
                && path.is_file(),
            "kernel artifact must be retained regular file"
        );
        ensure!(
            item["sha256"].as_str() == Some(digest(&path)?.as_str()),
            "kernel artifact digest differs: {name}"
        );
        paths.insert(name.clone(), path);
    }
    ensure!(
        read(&paths["provenance"])? == *provenance,
        "kernel provenance differs"
    );
    let mut elf = [0; 4];
    File::open(&paths["kernel"])?.read_exact(&mut elf)?;
    ensure!(elf == *b"\x7fELF", "FC kernel must be extracted ELF");
    Ok(paths)
}

pub(super) fn validate_output(stdout: &str, stderr: &str, guest: bool) -> Result<()> {
    ensure!(
        !format!("{stdout}\n{stderr}")
            .to_lowercase()
            .contains("kernel panic"),
        "kernel panic"
    );
    let lines: Vec<_> = stdout.lines().collect();
    let positions = |prefix: &str| {
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.starts_with(prefix).then_some(index))
            .collect::<Vec<_>>()
    };
    let ready = positions("REFERENCE_READY");
    let result = positions("REFERENCE_RESULT");
    let exit = positions("REFERENCE_EXIT");
    ensure!(
        ready.len() == 1 && lines[ready[0]] == "REFERENCE_READY" && result.len() == 1,
        "missing, malformed or duplicate startup markers"
    );
    ensure!(ready[0] < result[0], "startup markers out of order");
    let payload = lines[result[0]]
        .strip_prefix("REFERENCE_RESULT ")
        .context("malformed result")?;
    ensure!(
        serde_json::from_str::<Value>(payload)? == json!({"mode":"ready","correctness":"passed"}),
        "incorrect startup result"
    );
    if guest {
        ensure!(
            exit.len() == 1 && lines[exit[0]] == "REFERENCE_EXIT 0" && result[0] < exit[0],
            "guest missing unique ordered successful exit"
        );
    } else if !exit.is_empty() {
        bail!("unexpected guest exit marker");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const GOOD: &str =
        "REFERENCE_READY\nREFERENCE_RESULT {\"mode\":\"ready\",\"correctness\":\"passed\"}\n";
    #[test]
    fn output_protocol_rejects_all_bad_boundaries() {
        validate_output(GOOD, "", false).unwrap();
        validate_output(&format!("{GOOD}REFERENCE_EXIT 0\n"), "", true).unwrap();
        for (out, err, guest) in [
            (format!("{GOOD}REFERENCE_READY\n"), "", false),
            (GOOD.to_string(), "Kernel panic", false),
            (GOOD.to_string(), "", true),
            (format!("{GOOD}REFERENCE_EXIT 0\n"), "", false),
            (GOOD.replace("passed", "failed"), "", false),
            (
                GOOD.replace("REFERENCE_READY", "REFERENCE_READYx"),
                "",
                false,
            ),
        ] {
            assert!(validate_output(&out, &err, guest).is_err());
        }
    }

    fn reference_fixture(root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(root.join("rootfs/bench")).unwrap();
        let mut entries = Vec::new();
        for name in ASSETS.into_iter().chain([
            "rootfs/bench/reference_workload.py",
            "rootfs/bench/affinity",
        ]) {
            let path = root.join(name);
            fs::write(&path, name).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            entries.push(json!({"path":name,"mode":0o644,"bytes":name.len(),"sha256":digest(&path).unwrap()}));
        }
        save(root.join("input-manifest.json"), &entries).unwrap();
    }

    #[test]
    fn reference_inventory_covers_content_modes_and_extra_nodes() {
        let root = tempfile::tempdir().unwrap();
        reference_fixture(root.path());
        verify_inputs(root.path()).unwrap();
        fs::write(root.path().join("rootfs/extra"), "extra").unwrap();
        assert!(verify_inputs(root.path()).is_err());
        fs::remove_file(root.path().join("rootfs/extra")).unwrap();
        fs::write(root.path().join("rootfs/bench/affinity"), "mutated").unwrap();
        assert!(verify_inputs(root.path()).is_err());
    }

    #[test]
    fn empty_upper_requires_real_existing_directory_without_any_child() {
        let root = tempfile::tempdir().unwrap();
        let upper = root.path().join("upper");
        assert!(require_empty_upper(&upper).is_err());
        fs::create_dir(&upper).unwrap();
        require_empty_upper(&upper).unwrap();
        fs::create_dir(upper.join("empty-mutation")).unwrap();
        assert!(require_empty_upper(&upper).is_err());
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(&upper, &linked).unwrap();
        assert!(require_empty_upper(&linked).is_err());
    }

    #[test]
    fn reference_manifest_rejects_escape_duplicates_and_file_symlinks() {
        for invalid in ["../outside", "/absolute", "rootfs//bench", "rootfs/./bench"] {
            let root = tempfile::tempdir().unwrap();
            reference_fixture(root.path());
            let mut entries = read(root.path().join("input-manifest.json")).unwrap();
            entries[0]["path"] = json!(invalid);
            save(root.path().join("input-manifest.json"), &entries).unwrap();
            assert!(verify_inputs(root.path()).is_err(), "{invalid}");
        }
        let root = tempfile::tempdir().unwrap();
        reference_fixture(root.path());
        fs::remove_file(root.path().join("rootfs/bench/affinity")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", root.path().join("rootfs/bench/affinity")).unwrap();
        assert!(verify_inputs(root.path()).is_err());
    }

    #[test]
    fn stock_kernel_receipt_requires_independent_provenance_and_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let provenance = json!({"distribution":"test","package":"kernel","package_version":"1",
            "source_url":"https://example.invalid/source","origin":"official-distro-stock",
            "pvisor_firmware":false});
        save(root.path().join("provenance"), &provenance).unwrap();
        for name in ["kernel", "config", "vmlinuz", "extractor", "initrd"] {
            fs::write(
                root.path().join(name),
                if name == "kernel" {
                    b"\x7fELFtest".as_slice()
                } else {
                    b"fixture"
                },
            )
            .unwrap();
        }
        let mut artifacts = serde_json::Map::new();
        for name in [
            "kernel",
            "config",
            "provenance",
            "vmlinuz",
            "extractor",
            "initrd",
        ] {
            artifacts.insert(
                name.into(),
                json!({"file":name,"sha256":digest(&root.path().join(name)).unwrap()}),
            );
        }
        let receipt = root.path().join("receipt.json");
        let mut record = json!({"schema":"pvisor-firecracker-kernel/v1","variant":"fc-system",
            "provenance":provenance,"artifacts":artifacts});
        save(&receipt, &record).unwrap();
        verify_kernel(&receipt).unwrap();
        record["provenance"]["pvisor_firmware"] = true.into();
        save(&receipt, &record).unwrap();
        assert!(verify_kernel(&receipt).is_err());
        record["provenance"]["pvisor_firmware"] = false.into();
        save(&receipt, &record).unwrap();
        fs::write(root.path().join("kernel"), "tampered").unwrap();
        assert!(verify_kernel(&receipt).is_err());
    }
}

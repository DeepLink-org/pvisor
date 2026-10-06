//! Read-only resolution of a unified cgroup in this process's mount namespace.
use anyhow::{Context, ensure};
use std::path::{Component, PathBuf};

pub(super) fn current_cgroup_v2() -> anyhow::Result<PathBuf> {
    resolve(
        &std::fs::read_to_string("/proc/self/cgroup").context("read /proc/self/cgroup")?,
        &std::fs::read_to_string("/proc/self/mountinfo").context("read /proc/self/mountinfo")?,
    )
}

fn kernel_path(text: &str) -> anyhow::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            ensure!(i + 3 < bytes.len(), "invalid mountinfo escape");
            decoded.push(match &bytes[i + 1..i + 4] {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => anyhow::bail!("invalid mountinfo escape"),
            });
            i += 4;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(decoded));
    ensure!(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "unsafe kernel path"
    );
    Ok(path)
}

fn resolve(cgroups: &str, mountinfo: &str) -> anyhow::Result<PathBuf> {
    let mut lines = cgroups.lines();
    let path = lines
        .next()
        .and_then(|line| line.strip_prefix("0::"))
        .context("service limits require a unified cgroup v2 hierarchy")?;
    ensure!(
        lines.next().is_none(),
        "hybrid cgroup hierarchy is unsupported"
    );
    // Unlike mountinfo, /proc/self/cgroup contains raw paths.
    let group = PathBuf::from(path);
    ensure!(
        group.is_absolute()
            && !group
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
        "cgroup is outside the visible namespace"
    );
    let mut candidates = Vec::new();
    for line in mountinfo.lines() {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        if filesystem.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let fields: Vec<_> = fields.split_whitespace().collect();
        ensure!(fields.len() >= 6, "invalid cgroup mountinfo");
        let root = kernel_path(fields[3])?;
        let mount = kernel_path(fields[4])?;
        if let Ok(relative) = group.strip_prefix(&root) {
            candidates.push((root.components().count(), mount.join(relative)));
        }
    }
    candidates.sort_by_key(|value| std::cmp::Reverse(value.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .context("cgroup is not visible in this mount namespace")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_subtree_mount_and_escaped_mountpoint() {
        let mounts = "1 0 0:1 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n2 0 0:2 /tenant /delegated\\040group rw - cgroup2 cgroup rw\n";
        assert_eq!(
            resolve("0::/tenant/service\n", mounts).unwrap(),
            PathBuf::from("/delegated group/service")
        );
        assert_eq!(
            resolve("0::/tenant\n", mounts).unwrap(),
            PathBuf::from("/delegated group")
        );
    }
    #[test]
    fn rejects_non_unified_invisible_and_unsafe_paths() {
        let mounts = "1 0 0:1 /tenant /delegated rw - cgroup2 cgroup rw\n";
        for groups in [
            "1:cpu:/tenant",
            "0::/tenant\n1:cpu:/tenant",
            "0::/other",
            "0::/../tenant",
            "0::relative",
            "",
        ] {
            assert!(resolve(groups, mounts).is_err(), "{groups}");
        }
        assert!(
            resolve(
                "0::/tenant",
                "1 0 0:1 /tenant /bad\\999 rw - cgroup2 cgroup rw"
            )
            .is_err()
        );
    }
}

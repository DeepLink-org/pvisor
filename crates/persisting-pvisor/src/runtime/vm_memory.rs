//! Host-side memory residency for a single runner. No snapshot format: Linux
//! swap preserves anonymous pages and faults them back into the existing VM.
//! The supervisor and its Gateway/OverlayNet stay outside this cgroup.
use anyhow::{Context, ensure};
use persisting_control::overlay::VmMemorySample;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) struct VmMemory {
    path: PathBuf,
}

impl VmMemory {
    #[cfg(target_os = "linux")]
    pub(crate) fn create(parent: &Path) -> anyhow::Result<Self> {
        let parent = parent.canonicalize().context("resolve vm.cgroup_parent")?;
        let mount = Path::new("/sys/fs/cgroup").canonicalize()?;
        ensure!(
            parent.starts_with(&mount) && parent != mount,
            "vm.cgroup_parent must be a delegated directory below /sys/fs/cgroup"
        );
        let controllers = std::fs::read_to_string(parent.join("cgroup.subtree_control"))?;
        ensure!(
            controllers.split_whitespace().any(|s| s == "memory"),
            "vm.cgroup_parent must already delegate the memory controller; pVisor does not modify ancestor cgroups"
        );
        let path = parent.join(format!("pvisor-vm-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path).context("create runner memory cgroup")?;
        let memory = Self { path };
        memory.sample()?;
        memory.membership_file()?;
        OpenOptions::new()
            .write(true)
            .open(memory.path.join("memory.reclaim"))
            .context("memory.reclaim is unavailable or not delegated")?;
        Ok(memory)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn membership_file(&self) -> anyhow::Result<File> {
        Ok(OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))?)
    }

    pub(crate) fn sample(&self) -> anyhow::Result<VmMemorySample> {
        let read = |name: &str| -> anyhow::Result<u64> {
            Ok(std::fs::read_to_string(self.path.join(name))?
                .trim()
                .parse()?)
        };
        let stat = std::fs::read_to_string(self.path.join("memory.stat"))?;
        Ok(VmMemorySample {
            current_bytes: read("memory.current")?,
            swap_bytes: read("memory.swap.current")?,
            anon_bytes: stat_value(&stat, "anon")?,
            file_bytes: stat_value(&stat, "file")?,
        })
    }

    pub(crate) fn ensure_swap_enabled(&self) -> anyhow::Result<()> {
        ensure!(
            std::fs::read_to_string(self.path.join("memory.swap.max"))?.trim() != "0",
            "runner cgroup disables swap"
        );
        let info = std::fs::read_to_string("/proc/meminfo")?;
        ensure!(
            stat_value(&info, "SwapFree:")? > 0,
            "host has no available swap; guest RAM offload requires swap"
        );
        Ok(())
    }

    /// EAGAIN means partial reclaim, never VM corruption. swappiness=200 is
    /// supported by Linux 6.14; the newer 'max' spelling is intentionally unused.
    pub(crate) fn reclaim(&self, bytes: u64) -> std::io::Result<()> {
        OpenOptions::new()
            .write(true)
            .open(self.path.join("memory.reclaim"))?
            .write_all(format!("{bytes} swappiness=200").as_bytes())
    }
}

impl Drop for VmMemory {
    fn drop(&mut self) {
        // Never recursively delete or migrate remaining processes. A live
        // runner must be reaped by its executor before the final owner drops.
        if let Err(error) = std::fs::remove_dir(&self.path) {
            tracing::warn!(path = %self.path.display(), %error, "runner cgroup cleanup failed");
        }
    }
}

fn stat_value(input: &str, key: &str) -> anyhow::Result<u64> {
    input
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some(key))
                .then(|| fields.next())
                .flatten()
        })
        .with_context(|| format!("missing memory statistic {key}"))?
        .parse()
        .with_context(|| format!("invalid memory statistic {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_does_not_silently_turn_missing_fields_into_zero() {
        assert_eq!(stat_value("anon 4096\nfile 8192\n", "anon").unwrap(), 4096);
        assert_eq!(
            stat_value("SwapFree: 1024 kB\n", "SwapFree:").unwrap(),
            1024
        );
        assert!(stat_value("anon_thp 4096\n", "anon").is_err());
        assert!(stat_value("anon invalid\n", "anon").is_err());
    }
}

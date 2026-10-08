//! Observed native process mappings, not an admission budget or reclaim proof.
use crate::{AttemptId, RunId};
use anyhow::ensure;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryObservation<T> {
    Measured { usage: T },
    Unavailable { error: String },
}
impl<T> MemoryObservation<T> {
    pub fn from_result(result: anyhow::Result<T>) -> Self {
        match result {
            Ok(usage) => Self::Measured { usage },
            Err(error) => {
                let mut error = format!("{error:#}");
                if error.is_empty() {
                    error = "memory probe failed".into();
                }
                let mut end = error.len().min(1024);
                while !error.is_char_boundary(end) {
                    end -= 1;
                }
                error.truncate(end);
                Self::Unavailable { error }
            }
        }
    }
    fn validate_with(&self, validate: impl FnOnce(&T) -> anyhow::Result<()>) -> anyhow::Result<()> {
        match self {
            Self::Measured { usage } => validate(usage),
            Self::Unavailable { error } => {
                ensure!(
                    !error.is_empty() && error.len() <= 1024,
                    "invalid memory probe error"
                );
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMemory {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub process: ResidentMemory,
}
impl ProcessMemory {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.pid > 0 && self.start_time_ticks > 0 && self.process.mapped_bytes > 0,
            "invalid supervisor process identity or mappings"
        );
        self.process.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemMemory {
    pub host_boot_id: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub free_bytes: u64,
    pub cached_bytes: u64,
    pub buffers_bytes: u64,
    pub slab_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}
impl SystemMemory {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.host_boot_id.len() == 36
                && self.host_boot_id.bytes().enumerate().all(|(i, byte)| {
                    if [8, 13, 18, 23].contains(&i) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                }),
            "invalid host boot identity"
        );
        ensure!(
            self.total_bytes > 0
                && [
                    self.available_bytes,
                    self.free_bytes,
                    self.cached_bytes,
                    self.buffers_bytes,
                    self.slab_bytes
                ]
                .iter()
                .all(|v| *v <= self.total_bytes)
                && self.swap_free_bytes <= self.swap_total_bytes,
            "invalid system memory observation"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryLimit {
    Unlimited,
    Bytes(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupMemory {
    /// Resolved directory in the reporting process's mount namespace.
    pub directory: String,
    pub device: u64,
    pub inode: u64,
    pub current_bytes: u64,
    pub peak_bytes: Option<u64>,
    pub max: MemoryLimit,
    pub high: MemoryLimit,
    pub swap_current_bytes: Option<u64>,
    /// Kernel keys retain their documented units; not every stat is bytes.
    pub stat: std::collections::BTreeMap<String, u64>,
    pub events: std::collections::BTreeMap<String, u64>,
}
impl CgroupMemory {
    pub fn validate(&self) -> anyhow::Result<()> {
        let path = std::path::Path::new(&self.directory);
        ensure!(
            self.inode > 0
                && self.directory.len() <= 4096
                && !self.directory.contains('\0')
                && path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "invalid cgroup memory scope"
        );
        let valid = |counters: &std::collections::BTreeMap<String, u64>, max| {
            !counters.is_empty()
                && counters.len() <= max
                && counters.keys().all(|key| {
                    !key.is_empty()
                        && key.len() <= 64
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                })
        };
        ensure!(
            valid(&self.stat, 256)
                && self.stat.contains_key("anon")
                && self.stat.contains_key("file")
                && valid(&self.events, 32),
            "invalid cgroup memory counters"
        );
        // Kernel stats overlap, are batched and can change during the read.
        // Neither their sum nor current <= high/max/peak is a valid invariant.
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeMemorySample {
    pub sampled_at_unix_ms: u64,
    pub supervisor: MemoryObservation<ProcessMemory>,
    pub system: MemoryObservation<SystemMemory>,
    pub cgroup: MemoryObservation<CgroupMemory>,
}
impl NodeMemorySample {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.sampled_at_unix_ms > 0,
            "invalid node memory sample time"
        );
        self.supervisor.validate_with(ProcessMemory::validate)?;
        self.system.validate_with(SystemMemory::validate)?;
        self.cgroup.validate_with(CgroupMemory::validate)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentMemory {
    pub mapped_bytes: u64,
    pub rss_bytes: u64,
    pub pss_bytes: u64,
    pub private_clean_bytes: u64,
    pub private_dirty_bytes: u64,
    pub shared_clean_bytes: u64,
    pub shared_dirty_bytes: u64,
    pub swap_bytes: u64,
    pub swap_pss_bytes: u64,
    // Linux excludes hugetlb pages from RSS/PSS; keep them visible separately.
    pub private_hugetlb_bytes: u64,
    pub shared_hugetlb_bytes: u64,
}
impl ResidentMemory {
    pub fn validate(&self) -> anyhow::Result<()> {
        let resident = self
            .private_clean_bytes
            .checked_add(self.private_dirty_bytes)
            .and_then(|n| n.checked_add(self.shared_clean_bytes))
            .and_then(|n| n.checked_add(self.shared_dirty_bytes));
        let accounted = self
            .rss_bytes
            .checked_add(self.swap_bytes)
            .and_then(|n| n.checked_add(self.private_hugetlb_bytes))
            .and_then(|n| n.checked_add(self.shared_hugetlb_bytes));
        ensure!(
            resident == Some(self.rss_bytes)
                && self.pss_bytes <= self.rss_bytes
                && accounted.is_some_and(|n| n <= self.mapped_bytes)
                && self.swap_pss_bytes <= self.swap_bytes,
            "inconsistent resident memory counters"
        );
        Ok(())
    }
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        Some(Self {
            mapped_bytes: self.mapped_bytes.checked_add(other.mapped_bytes)?,
            rss_bytes: self.rss_bytes.checked_add(other.rss_bytes)?,
            pss_bytes: self.pss_bytes.checked_add(other.pss_bytes)?,
            private_clean_bytes: self
                .private_clean_bytes
                .checked_add(other.private_clean_bytes)?,
            private_dirty_bytes: self
                .private_dirty_bytes
                .checked_add(other.private_dirty_bytes)?,
            shared_clean_bytes: self
                .shared_clean_bytes
                .checked_add(other.shared_clean_bytes)?,
            shared_dirty_bytes: self
                .shared_dirty_bytes
                .checked_add(other.shared_dirty_bytes)?,
            swap_bytes: self.swap_bytes.checked_add(other.swap_bytes)?,
            swap_pss_bytes: self.swap_pss_bytes.checked_add(other.swap_pss_bytes)?,
            private_hugetlb_bytes: self
                .private_hugetlb_bytes
                .checked_add(other.private_hugetlb_bytes)?,
            shared_hugetlb_bytes: self
                .shared_hugetlb_bytes
                .checked_add(other.shared_hugetlb_bytes)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeVmMemory {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub process: ResidentMemory,
    pub guest_ram: ResidentMemory,
    /// Other VMM process mappings; excludes kernel/KVM allocations, supervisor,
    /// pager caches and any other process's mappings.
    pub non_ram: ResidentMemory,
}
impl NativeVmMemory {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.pid > 0 && self.start_time_ticks > 0 && self.guest_ram.mapped_bytes > 0,
            "invalid native VM memory identity or unmapped RAM"
        );
        self.process.validate()?;
        self.guest_ram.validate()?;
        self.non_ram.validate()?;
        ensure!(
            self.guest_ram.checked_add(&self.non_ram).as_ref() == Some(&self.process),
            "native memory partitions do not match process counters"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMemorySample {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub sampled_at_unix_ms: u64,
    pub usage: Option<NativeVmMemory>,
    pub error: Option<String>,
}
impl RunMemorySample {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.run_id.is_empty()
                && !self.attempt_id.is_empty()
                && self.run_id.as_str().len() <= 256
                && self.attempt_id.as_str().len() <= 256
                && self.sampled_at_unix_ms > 0,
            "invalid memory sample identity"
        );
        match (&self.usage, &self.error) {
            (Some(usage), None) => usage.validate()?,
            (None, Some(error)) => ensure!(
                !error.is_empty() && error.len() <= 1024,
                "invalid memory probe error"
            ),
            _ => anyhow::bail!("memory sample requires exactly one usage or error"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_memory_keeps_overlapping_scopes_and_partial_errors_explicit() {
        let valid = NodeMemorySample {
            sampled_at_unix_ms: 1,
            supervisor: MemoryObservation::Measured {
                usage: ProcessMemory {
                    pid: 123,
                    start_time_ticks: 99,
                    process: ResidentMemory {
                        mapped_bytes: 8192,
                        rss_bytes: 4096,
                        pss_bytes: 4096,
                        private_dirty_bytes: 4096,
                        ..Default::default()
                    },
                },
            },
            system: MemoryObservation::Measured {
                usage: SystemMemory {
                    host_boot_id: "00000000-0000-0000-0000-000000000001".into(),
                    total_bytes: 10000,
                    available_bytes: 4000,
                    free_bytes: 1000,
                    cached_bytes: 3000,
                    buffers_bytes: 200,
                    slab_bytes: 1000,
                    swap_total_bytes: 100,
                    swap_free_bytes: 50,
                },
            },
            cgroup: MemoryObservation::Measured {
                usage: CgroupMemory {
                    directory: "/sys/fs/cgroup/worker".into(),
                    device: 30,
                    inode: 7,
                    current_bytes: 5000,
                    peak_bytes: Some(4900),
                    max: MemoryLimit::Bytes(4000),
                    high: MemoryLimit::Bytes(3000),
                    swap_current_bytes: Some(10),
                    stat: [
                        ("anon".into(), 1000),
                        ("file".into(), 3000),
                        ("kernel".into(), 1000),
                        ("slab".into(), 500),
                        ("future_counter".into(), 7),
                    ]
                    .into(),
                    events: [("high".into(), 1), ("oom_kill".into(), 0)].into(),
                },
            },
        };
        valid.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<NodeMemorySample>(serde_json::to_value(&valid).unwrap())
                .unwrap(),
            valid
        );
        for case in 0..8 {
            let mut bad = valid.clone();
            match case {
                0 => bad.sampled_at_unix_ms = 0,
                1 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.supervisor {
                        usage.pid = 0;
                    }
                }
                2 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.supervisor {
                        usage.process.pss_bytes = 8192;
                    }
                }
                3 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.system {
                        usage.host_boot_id = "invalid".into();
                    }
                }
                4 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.system {
                        usage.available_bytes = 10001;
                    }
                }
                5 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.cgroup {
                        usage.directory = "/sys/fs/cgroup/../hidden".into();
                    }
                }
                6 => {
                    if let MemoryObservation::Measured { usage } = &mut bad.cgroup {
                        usage.stat.remove("file");
                    }
                }
                _ => {
                    bad.cgroup = MemoryObservation::Unavailable {
                        error: "x".repeat(1025),
                    }
                }
            }
            assert!(bad.validate().is_err(), "{case}");
        }
        let mut partial = valid.clone();
        partial.cgroup =
            MemoryObservation::from_result(Err(anyhow::anyhow!("unresolvable namespace")));
        partial.validate().unwrap();
        let mut json = serde_json::to_value(&partial).unwrap();
        json["cgroup"]["usage"] = serde_json::json!({});
        assert!(serde_json::from_value::<NodeMemorySample>(json).is_err());
        let mut json = serde_json::to_value(&valid).unwrap();
        json["system"]["usage"]["extra"] = true.into();
        assert!(serde_json::from_value::<NodeMemorySample>(json).is_err());
        let error: MemoryObservation<SystemMemory> =
            MemoryObservation::from_result(Err(anyhow::anyhow!("界".repeat(1000))));
        error.validate_with(SystemMemory::validate).unwrap();
        MemoryObservation::<SystemMemory>::from_result(Err(anyhow::anyhow!("")))
            .validate_with(SystemMemory::validate)
            .unwrap();
    }
    #[test]
    fn partitions_errors_and_unknown_fields_are_strict_without_treating_failure_as_zero() {
        let ram = ResidentMemory {
            mapped_bytes: 16384,
            rss_bytes: 8192,
            pss_bytes: 4096,
            shared_clean_bytes: 8192,
            ..Default::default()
        };
        let non_ram = ResidentMemory {
            mapped_bytes: 4096,
            rss_bytes: 4096,
            pss_bytes: 4096,
            private_dirty_bytes: 4096,
            ..Default::default()
        };
        let valid = RunMemorySample {
            run_id: "run".into(),
            attempt_id: "attempt".into(),
            sampled_at_unix_ms: 1,
            usage: Some(NativeVmMemory {
                pid: 123,
                start_time_ticks: 99,
                process: ram.checked_add(&non_ram).unwrap(),
                guest_ram: ram,
                non_ram,
            }),
            error: None,
        };
        valid.validate().unwrap();
        for case in 0..9 {
            let mut bad = valid.clone();
            let usage = bad.usage.as_mut().unwrap();
            match case {
                0 => usage.guest_ram.pss_bytes = 16384,
                1 => usage.non_ram.private_dirty_bytes = 0,
                2 => usage.process.mapped_bytes += 4096,
                3 => usage.pid = 0,
                4 => usage.start_time_ticks = 0,
                5 => bad.error = Some("probe failed".into()),
                6 => usage.guest_ram.private_hugetlb_bytes = u64::MAX,
                7 => usage.guest_ram.swap_bytes = 16384,
                _ => bad.sampled_at_unix_ms = 0,
            }
            assert!(bad.validate().is_err(), "case {case}");
        }
        let mut failed = valid.clone();
        failed.usage = None;
        failed.error = Some("process exited during sampling".into());
        failed.validate().unwrap();
        failed.error = None;
        assert!(failed.validate().is_err());
        failed.error = Some("x".repeat(1025));
        assert!(failed.validate().is_err());
        let mut json = serde_json::to_value(&valid).unwrap();
        json["usage"]["guest_ram"]["unchecked"] = true.into();
        assert!(serde_json::from_value::<RunMemorySample>(json).is_err());
        let huge = ResidentMemory {
            mapped_bytes: u64::MAX,
            ..Default::default()
        };
        assert!(huge.checked_add(&valid.usage.unwrap().guest_ram).is_none());
    }
}

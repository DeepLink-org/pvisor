//! Node-local admission based on reservations plus read-only Linux signals.
//! No observation here authorizes memory overcommit or releasing a task charge.
use crate::{AdmissionBlock, AdmissionMode, AdmissionReport, NodeMeasurements, Resources};
use anyhow::{Context, ensure};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdmissionPolicy {
    pub mode: AdmissionMode,
    pub memory_reserve_bytes: u64,
    /// 100 basis points = one percent of stalled wall time, not CPU usage.
    pub cpu_some_avg10_limit_bps: u16,
    pub memory_full_avg10_limit_bps: u16,
    pub max_sample_age_ms: u64,
    pub cpu_overcommit_bps: u16,
}
impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            mode: AdmissionMode::Reservations,
            memory_reserve_bytes: 256 * 1024 * 1024,
            cpu_some_avg10_limit_bps: 5000,
            memory_full_avg10_limit_bps: 100,
            max_sample_age_ms: 3000,
            cpu_overcommit_bps: 10_000,
        }
    }
}
impl AdmissionPolicy {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            (10_000..=40_000).contains(&self.cpu_overcommit_bps)
                && (self.cpu_overcommit_bps == 10_000 || self.mode == AdmissionMode::LinuxPressure),
            "CPU overcommit requires Linux pressure admission and a 10000..40000 basis point ratio"
        );
        ensure!(
            self.cpu_some_avg10_limit_bps > 0 && self.cpu_some_avg10_limit_bps <= 10_000,
            "CPU pressure threshold must be 1..10000 basis points"
        );
        ensure!(
            self.memory_full_avg10_limit_bps > 0 && self.memory_full_avg10_limit_bps <= 10_000,
            "memory pressure threshold must be 1..10000 basis points"
        );
        ensure!(
            self.max_sample_age_ms > 0 && self.max_sample_age_ms <= 30_000,
            "sample age limit must be 1..30000 ms"
        );
        Ok(())
    }

    pub fn report(
        &self,
        capacity: Resources,
        used: Resources,
        sample_age_ms: u64,
        sample: Result<NodeMeasurements, String>,
    ) -> anyhow::Result<AdmissionReport> {
        self.validate()?;
        let mut report = AdmissionReport {
            mode: self.mode,
            cpu_overcommit_bps: self.cpu_overcommit_bps,
            sample_age_ms: 0,
            available: capacity.checked_sub(used).context("worker over capacity")?,
            measurements: None,
            blocked: Vec::new(),
            error: None,
        };
        if self.mode == AdmissionMode::Reservations {
            return Ok(report);
        }
        report.sample_age_ms = sample_age_ms;
        let sample = sample.and_then(|mut measured| {
            if self.cpu_overcommit_bps > 10_000
                && !measured
                    .local_cpu_quota_millis
                    .is_some_and(|quota| quota > 0 && measured.cpu_limit_millis <= quota)
            {
                return Err("CPU overcommit requires a finite local cpu.max quota".into());
            }
            // Preserve the legacy report shape for the default policy.
            if self.cpu_overcommit_bps == 10_000 {
                measured.local_cpu_quota_millis = None;
            }
            Ok(measured)
        });
        let measurements = match sample {
            Ok(m) => m,
            Err(mut error) => {
                if error.is_empty() {
                    error = "node probe failed".into();
                }
                let mut end = error.len().min(4096);
                while !error.is_char_boundary(end) {
                    end -= 1;
                }
                error.truncate(end);
                report.error = Some(error);
                report.blocked.push(AdmissionBlock::ProbeFailed);
                report.available = Resources::default();
                report.validate()?;
                return Ok(report);
            }
        };
        let headroom = measurements
            .cgroup_memory_headroom_bytes
            .map_or(measurements.system_memory_available_bytes, |v| {
                v.min(measurements.system_memory_available_bytes)
            })
            .saturating_sub(self.memory_reserve_bytes);
        report.available.memory_bytes = report.available.memory_bytes.min(headroom);
        let reserved_cpu_limit = u64::try_from(
            u128::from(measurements.cpu_limit_millis) * u128::from(self.cpu_overcommit_bps)
                / 10_000,
        )
        .context("CPU reservation limit overflow")?;
        report.available.cpu_millis = report
            .available
            .cpu_millis
            .min(reserved_cpu_limit.saturating_sub(used.cpu_millis));
        if headroom == 0 {
            report.blocked.push(AdmissionBlock::MemoryHeadroom);
            report.available.cpu_millis = 0; // offloaded resume can fault RAM in
        }
        if reserved_cpu_limit <= used.cpu_millis {
            report.blocked.push(AdmissionBlock::CpuQuota);
        }
        if measurements.cpu_some_avg10_bps >= self.cpu_some_avg10_limit_bps {
            report.blocked.push(AdmissionBlock::CpuPressure);
            report.available.cpu_millis = 0;
        }
        if measurements.memory_full_avg10_bps >= self.memory_full_avg10_limit_bps {
            report.blocked.push(AdmissionBlock::MemoryPressure);
            report.available.memory_bytes = 0;
            report.available.cpu_millis = 0;
        }
        report.measurements = Some(measurements);
        if sample_age_ms >= self.max_sample_age_ms {
            report.blocked.push(AdmissionBlock::StaleSample);
            report.available = Resources::default();
        }
        report.validate()?;
        Ok(report)
    }
}

/// Reads only real kernel interfaces. A v1/hybrid hierarchy or a cgroup not
/// resolvable in this mount namespace fails rather than ignoring its limits.
pub fn sample_linux() -> anyhow::Result<NodeMeasurements> {
    ensure!(
        cfg!(target_os = "linux"),
        "Linux pressure admission requires Linux"
    );
    sample_from_proc(Path::new("/proc"))
}

/// Resolve this process's cgroup v2 directory in its visible mount namespace.
/// Read-only; callers separately verify delegation before changing it.
pub fn current_cgroup_v2() -> anyhow::Result<PathBuf> {
    cgroup_paths(
        &read(Path::new("/proc/self/cgroup"))?,
        &read(Path::new("/proc/self/mountinfo"))?,
    )
    .map(|(group, _)| group)
}

fn read(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}
fn optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}
fn number(text: &str) -> anyhow::Result<u64> {
    text.trim().parse().context("invalid kernel counter")
}
fn mem_available(text: &str) -> anyhow::Result<u64> {
    let line = text
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .context("missing MemAvailable")?;
    let values: Vec<_> = line.split_whitespace().collect();
    ensure!(
        values.len() == 3 && values[2] == "kB",
        "invalid MemAvailable unit"
    );
    number(values[1])?
        .checked_mul(1024)
        .context("MemAvailable overflow")
}
fn pressure(text: &str, class: &str) -> anyhow::Result<u16> {
    let line = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some(class))
        .context("missing PSI class")?;
    let value = line
        .split_whitespace()
        .find_map(|v| v.strip_prefix("avg10="))
        .context("missing PSI avg10")?;
    let percentage = value.parse::<f64>().context("invalid PSI percentage")?;
    ensure!(
        percentage.is_finite() && (0.0..=100.0).contains(&percentage),
        "invalid PSI percentage"
    );
    Ok((percentage * 100.0).round() as u16)
}

fn cpus(text: &str) -> anyhow::Result<u64> {
    let mut total = 0_u64;
    let mut previous_end = None;
    for group in text.trim().split(',') {
        let (first, last) = match group.split_once('-') {
            Some((first, last)) => (number(first)?, number(last)?),
            None => {
                let v = number(group)?;
                (v, v)
            }
        };
        ensure!(
            first <= last && previous_end.is_none_or(|p| first > p),
            "invalid CPU set"
        );
        total = total
            .checked_add(
                last.checked_sub(first)
                    .and_then(|n| n.checked_add(1))
                    .context("CPU set overflow")?,
            )
            .context("CPU set overflow")?;
        previous_end = Some(last);
    }
    ensure!(total > 0, "empty CPU set");
    total.checked_mul(1000).context("CPU capacity overflow")
}
fn cpu_quota(text: &str) -> anyhow::Result<Option<u64>> {
    let values: Vec<_> = text.split_whitespace().collect();
    ensure!(values.len() == 2, "invalid cpu.max");
    let period = number(values[1])?;
    ensure!(period > 0, "zero CPU period");
    if values[0] == "max" {
        return Ok(None);
    }
    let quota = number(values[0])?;
    let millis = (quota as u128 * 1000) / period as u128;
    Ok(Some(u64::try_from(millis).context("CPU quota overflow")?))
}

fn kernel_path(text: &str) -> anyhow::Result<PathBuf> {
    // mountinfo uses octal escapes for space, tab, newline and backslash.
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            ensure!(i + 3 < bytes.len(), "invalid mountinfo escape");
            let escape = &bytes[i + 1..i + 4];
            let byte = match escape {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => anyhow::bail!("invalid mountinfo escape"),
            };
            decoded.push(byte);
            i += 4;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(decoded))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(String::from_utf8(decoded)?);
    ensure!(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "unsafe kernel path"
    );
    Ok(path)
}

pub(crate) fn cgroup_paths(cgroups: &str, mountinfo: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let mut lines = cgroups.lines();
    let path = lines
        .next()
        .and_then(|l| l.strip_prefix("0::"))
        .context("pressure admission requires a unified cgroup v2 hierarchy")?;
    ensure!(
        lines.next().is_none(),
        "hybrid cgroup hierarchy is unsupported"
    );
    // /proc/self/cgroup paths are raw, not mountinfo-escaped.
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
            candidates.push((root.components().count(), mount.join(relative), mount));
        }
    }
    candidates.sort_by_key(|v| std::cmp::Reverse(v.0));
    let (_, leaf, mount) = candidates
        .into_iter()
        .next()
        .context("cgroup is not visible in this mount namespace")?;
    Ok((leaf, mount))
}

fn sample_from_proc(proc: &Path) -> anyhow::Result<NodeMeasurements> {
    let system_memory_available_bytes = mem_available(&read(&proc.join("meminfo"))?)?;
    let mut cpu_some_avg10_bps = pressure(&read(&proc.join("pressure/cpu"))?, "some")?;
    let mut memory_full_avg10_bps = pressure(&read(&proc.join("pressure/memory"))?, "full")?;
    let status = read(&proc.join("self/status"))?;
    let affinity = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .context("missing CPU affinity")?;
    let mut cpu_limit_millis = cpus(affinity)?;
    let (mut group, mount) = cgroup_paths(
        &read(&proc.join("self/cgroup"))?,
        &read(&proc.join("self/mountinfo"))?,
    )?;
    ensure!(
        std::fs::metadata(&group)
            .with_context(|| format!("stat {}", group.display()))?
            .is_dir(),
        "resolved cgroup is not a directory"
    );
    let mut cgroup_memory_headroom_bytes: Option<u64> = None;
    let mut local_cpu_quota_millis = None;
    for depth in 0..=256 {
        ensure!(depth < 256, "cgroup hierarchy too deep");
        // Root groups may omit controller limit files. Missing non-root
        // controller files mean that controller is not enabled there.
        for limit in ["memory.max", "memory.high"] {
            if let Some(max) = optional(&group.join(limit))?
                && max.trim() != "max"
            {
                let max = number(&max)?;
                let current = number(&read(&group.join("memory.current"))?)?;
                let headroom = max.saturating_sub(current);
                cgroup_memory_headroom_bytes =
                    Some(cgroup_memory_headroom_bytes.map_or(headroom, |v| v.min(headroom)));
            }
        }
        if let Some(max) = optional(&group.join("cpu.max"))?
            && let Some(limit) = cpu_quota(&max)?
        {
            if depth == 0 {
                local_cpu_quota_millis = Some(limit);
            }
            cpu_limit_millis = cpu_limit_millis.min(limit);
        }
        if let Some(set) = optional(&group.join("cpuset.cpus.effective"))?
            && !set.trim().is_empty()
        {
            cpu_limit_millis = cpu_limit_millis.min(cpus(&set)?);
        }
        // Unlike global CPU "full", CPU "some" is meaningful at both scopes.
        if let Some(value) = optional(&group.join("cpu.pressure"))? {
            cpu_some_avg10_bps = cpu_some_avg10_bps.max(pressure(&value, "some")?);
        }
        if let Some(value) = optional(&group.join("memory.pressure"))? {
            memory_full_avg10_bps = memory_full_avg10_bps.max(pressure(&value, "full")?);
        }
        if group == mount {
            break;
        }
        ensure!(
            group.pop() && group.starts_with(&mount),
            "invalid cgroup ancestor"
        );
    }
    Ok(NodeMeasurements {
        system_memory_available_bytes,
        cgroup_memory_headroom_bytes,
        cpu_limit_millis,
        local_cpu_quota_millis,
        cpu_some_avg10_bps,
        memory_full_avg10_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cpu_overcommit_requires_local_quota_and_preserves_pressure_age_and_memory_gates() {
        let policy = AdmissionPolicy {
            mode: AdmissionMode::LinuxPressure,
            cpu_overcommit_bps: 20_000,
            memory_reserve_bytes: 100,
            ..Default::default()
        };
        let capacity = Resources {
            slots: 4,
            cpu_millis: 4000,
            memory_bytes: 8000,
        };
        let used = Resources {
            slots: 1,
            cpu_millis: 1000,
            memory_bytes: 2000,
        };
        let measured = NodeMeasurements {
            system_memory_available_bytes: 6000,
            cgroup_memory_headroom_bytes: Some(3000),
            cpu_limit_millis: 1000,
            local_cpu_quota_millis: Some(1000),
            cpu_some_avg10_bps: 0,
            memory_full_avg10_bps: 0,
        };
        let report = policy
            .report(capacity, used, 0, Ok(measured.clone()))
            .unwrap();
        assert_eq!(report.available.cpu_millis, 1000);
        assert_eq!(report.available.memory_bytes, 2900);
        assert_eq!(report.cpu_reservation_limit_millis(), Some(2000));
        assert_eq!(report.measurements.as_ref().unwrap().cpu_limit_millis, 1000);
        let default = AdmissionPolicy {
            mode: AdmissionMode::LinuxPressure,
            memory_reserve_bytes: 100,
            ..Default::default()
        }
        .report(capacity, used, 0, Ok(measured.clone()))
        .unwrap();
        assert_eq!(default.available.cpu_millis, 0);
        let legacy = serde_json::to_value(default).unwrap();
        assert!(legacy.get("cpu_overcommit_bps").is_none());
        assert!(
            legacy["measurements"]
                .get("local_cpu_quota_millis")
                .is_none()
        );
        serde_json::from_value::<AdmissionReport>(legacy)
            .unwrap()
            .validate()
            .unwrap();
        for case in 0..7 {
            let mut bad = measured.clone();
            let mut age = 0;
            match case {
                0 => bad.local_cpu_quota_millis = None,
                1 => bad.local_cpu_quota_millis = Some(0),
                2 => bad.local_cpu_quota_millis = Some(999),
                3 => bad.cpu_some_avg10_bps = policy.cpu_some_avg10_limit_bps,
                4 => bad.memory_full_avg10_bps = policy.memory_full_avg10_limit_bps,
                5 => bad.cgroup_memory_headroom_bytes = Some(100),
                _ => age = policy.max_sample_age_ms,
            }
            let failed = policy.report(capacity, used, age, Ok(bad)).unwrap();
            assert_eq!(failed.available.cpu_millis, 0, "case {case}");
            if case <= 2 {
                assert!(
                    failed.error.is_some() && failed.blocked.contains(&AdmissionBlock::ProbeFailed)
                );
            }
        }
        let mut invalid = policy.clone();
        invalid.mode = AdmissionMode::Reservations;
        assert!(invalid.validate().is_err());
        for ratio in [0, 9999, 40001] {
            invalid = policy.clone();
            invalid.cpu_overcommit_bps = ratio;
            assert!(invalid.validate().is_err());
        }
        let mut malformed = report.clone();
        malformed.available.cpu_millis = 2001;
        assert!(malformed.validate().is_err());
        let mut overflow = measured;
        overflow.cpu_limit_millis = u64::MAX;
        overflow.local_cpu_quota_millis = Some(u64::MAX);
        assert!(policy.report(capacity, used, 0, Ok(overflow)).is_err());
    }

    #[test]
    fn kernel_parsers_reject_missing_nonfinite_overflow_and_unsafe_paths() {
        assert_eq!(mem_available("MemAvailable: 123 kB").unwrap(), 123 * 1024);
        assert!(mem_available("MemAvailable: 123 MB").is_err());
        assert!(mem_available("MemAvailable: 18446744073709551615 kB").is_err());
        for value in ["NaN", "inf", "-1", "100.01"] {
            assert!(pressure(&format!("some avg10={value}"), "some").is_err());
        }
        assert_eq!(pressure("some avg10=0.17 avg60=1.00", "some").unwrap(), 17);
        assert_eq!(cpus("0-3,6,8-9").unwrap(), 7000);
        assert!(cpus("0-3,2").is_err());
        assert_eq!(cpu_quota("25000 100000").unwrap(), Some(250));
        assert_eq!(cpu_quota("max 100000").unwrap(), None);
        assert!(cpu_quota("max 0").is_err());
        assert!(kernel_path("/a/../b").is_err());
        assert_eq!(kernel_path("/a\\040b").unwrap(), Path::new("/a b"));
        assert!(cgroup_paths("0::/../../outside", "").is_err());
        assert!(cgroup_paths("0::/\n1:memory:/", "").is_err());
    }

    #[test]
    fn probe_intersects_visible_ancestors_and_mount_root_instead_of_guessing_path() {
        let temp = tempfile::tempdir().unwrap();
        let proc = temp.path().join("proc");
        let mount = temp.path().join("cgroup mount");
        let parent = mount.join("parent");
        let leaf = parent.join("leaf");
        for path in [proc.join("self"), proc.join("pressure"), leaf.clone()] {
            std::fs::create_dir_all(path).unwrap();
        }
        let put = |path: &Path, value: &str| std::fs::write(path, value).unwrap();
        put(&proc.join("meminfo"), "MemAvailable: 4096 kB\n");
        put(&proc.join("self/status"), "Cpus_allowed_list:\t0-7\n");
        put(&proc.join("self/cgroup"), "0::/host/parent/leaf\n");
        put(
            &proc.join("self/mountinfo"),
            &format!(
                "1 0 0:1 /host {} rw - cgroup2 cgroup rw\n",
                mount.display().to_string().replace(' ', "\\040")
            ),
        );
        for path in [proc.join("pressure/cpu"), leaf.join("cpu.pressure")] {
            put(&path, "some avg10=2.00\nfull avg10=0.00\n");
        }
        put(
            &proc.join("pressure/memory"),
            "some avg10=5.00\nfull avg10=0.10\n",
        );
        put(
            &parent.join("memory.pressure"),
            "some avg10=8.00\nfull avg10=0.30\n",
        );
        put(&leaf.join("memory.max"), "10485760");
        put(&leaf.join("memory.current"), "100");
        put(&parent.join("memory.max"), "2000");
        put(&parent.join("memory.current"), "1500");
        put(&leaf.join("cpu.max"), "max 100000");
        put(&parent.join("cpu.max"), "25000 100000");
        let measured = sample_from_proc(&proc).unwrap();
        assert_eq!(measured.cgroup_memory_headroom_bytes, Some(500));
        assert_eq!(measured.cpu_limit_millis, 250);
        assert_eq!(measured.cpu_some_avg10_bps, 200);
        assert_eq!(measured.memory_full_avg10_bps, 30);
        put(&leaf.join("memory.high"), "120");
        assert_eq!(
            sample_from_proc(&proc)
                .unwrap()
                .cgroup_memory_headroom_bytes,
            Some(20)
        );
        // Current values beyond a newly reduced max yield zero, not underflow.
        put(&parent.join("memory.current"), "2500");
        assert_eq!(
            sample_from_proc(&proc)
                .unwrap()
                .cgroup_memory_headroom_bytes,
            Some(0)
        );
        put(&parent.join("cpu.max"), "broken");
        assert!(sample_from_proc(&proc).is_err());
    }

    #[test]
    fn pressure_limits_only_admission_and_failure_or_staleness_stops_resume() {
        let policy = AdmissionPolicy {
            mode: AdmissionMode::LinuxPressure,
            memory_reserve_bytes: 100,
            ..Default::default()
        };
        let capacity = Resources {
            slots: 8,
            memory_bytes: 8000,
            cpu_millis: 4000,
        };
        let used = Resources {
            slots: 2,
            memory_bytes: 2000,
            cpu_millis: 250,
        };
        let measured = NodeMeasurements {
            local_cpu_quota_millis: None,
            system_memory_available_bytes: 5000,
            cgroup_memory_headroom_bytes: Some(1000),
            cpu_limit_millis: 1000,
            cpu_some_avg10_bps: 0,
            memory_full_avg10_bps: 0,
        };
        let report = policy
            .report(capacity, used, 0, Ok(measured.clone()))
            .unwrap();
        assert_eq!(
            report.available,
            Resources {
                slots: 6,
                memory_bytes: 900,
                cpu_millis: 750
            }
        );
        let stale = policy
            .report(
                capacity,
                used,
                policy.max_sample_age_ms,
                Ok(measured.clone()),
            )
            .unwrap();
        assert_eq!(stale.available, Resources::default());
        assert!(stale.blocked.contains(&AdmissionBlock::StaleSample));
        let failed = policy
            .report(capacity, used, 0, Err("probe inaccessible".into()))
            .unwrap();
        assert_eq!(failed.available, Resources::default());
        let mut pressured = measured.clone();
        pressured.memory_full_avg10_bps = policy.memory_full_avg10_limit_bps;
        let report = policy.report(capacity, used, 0, Ok(pressured)).unwrap();
        assert_eq!(report.available.memory_bytes, 0);
        assert_eq!(report.available.cpu_millis, 0);
        assert_eq!(report.available.slots, 6);
        let mut no_headroom = measured;
        no_headroom.cgroup_memory_headroom_bytes = Some(99);
        assert_eq!(
            policy
                .report(capacity, used, 0, Ok(no_headroom))
                .unwrap()
                .available
                .cpu_millis,
            0
        );
        let reservations = AdmissionPolicy::default()
            .report(capacity, used, 99999, Err("unused probe".into()))
            .unwrap();
        assert_eq!(reservations.available, capacity.checked_sub(used).unwrap());
        assert!(reservations.measurements.is_none());
    }
}

//! Cgroup v2 paths and values.
//!
//! Limit values are pre-rendered by [`crate::plan`]; this module owns the
//! filesystem layout: where the unified controller is mounted, which files a
//! `CgroupPlan` produces, and how the systemd `slice:prefix:id` notation maps
//! onto directories.

use crate::plan::CgroupPlan;

/// Default mount point of the cgroup v2 unified hierarchy.
pub const UNIFIED_MOUNT: &str = "/sys/fs/cgroup";

/// Files (relative to the cgroup directory) written for a plan, in order.
pub fn control_files(plan: &CgroupPlan) -> Vec<(&'static str, String)> {
    let mut files = Vec::new();
    if let Some(value) = plan.pids_max.as_deref() {
        files.push(("pids.max", value.to_string()));
    }
    if let Some(value) = plan.memory_max.as_deref() {
        files.push(("memory.max", value.to_string()));
    }
    if let Some(value) = plan.cpu_max.as_deref() {
        files.push(("cpu.max", value.to_string()));
    }
    files
}

/// Absolute directory of the cgroup for a plan, when one is configured.
pub fn cgroup_dir(plan: &CgroupPlan) -> Option<std::path::PathBuf> {
    let path = plan.path.as_deref()?;
    if path.is_empty() {
        return None;
    }
    Some(std::path::Path::new(UNIFIED_MOUNT).join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::CgroupPlan;

    #[test]
    fn limits_render_as_ordered_files() {
        let plan = CgroupPlan {
            path: Some("burstable.slice/pod1/c1".to_string()),
            pids_max: Some("128".to_string()),
            memory_max: Some("max".to_string()),
            cpu_max: Some("500000 100000".to_string()),
        };
        let files = control_files(&plan);
        assert_eq!(
            files,
            vec![
                ("pids.max", "128".to_string()),
                ("memory.max", "max".to_string()),
                ("cpu.max", "500000 100000".to_string()),
            ]
        );
        assert_eq!(
            cgroup_dir(&plan),
            Some(std::path::PathBuf::from(
                "/sys/fs/cgroup/burstable.slice/pod1/c1"
            ))
        );
    }

    #[test]
    fn no_path_means_no_directory() {
        let plan = CgroupPlan {
            path: None,
            ..CgroupPlan::default()
        };
        assert!(cgroup_dir(&plan).is_none());
        let empty = CgroupPlan {
            path: Some(String::new()),
            ..CgroupPlan::default()
        };
        assert!(cgroup_dir(&empty).is_none());
    }
}

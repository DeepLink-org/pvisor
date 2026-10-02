//! The container plan: everything the init child needs, derived once from the
//! OCI spec plus the containerd `Create` request.
//!
//! The plan is a plain serializable value so the Linux child process can
//! consume it after re-exec (house "self-exec" pattern) and so the derivation
//! logic stays unit-testable on any host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::spec::ANNOTATION_PREFIX;
use oci_spec::runtime::{LinuxNamespaceType, Spec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("bundle config.json has no process section")]
    MissingProcess,
    #[error("bundle process section has no args")]
    MissingArgs,
    #[error("VM process constraint is not supported: {0}")]
    UnsupportedVmProcess(&'static str),
    #[error("namespace type {0} is not supported (M1 limitation)")]
    UnsupportedNamespaceType(String),
    #[error("unsupported mount type {0} at {1}")]
    UnsupportedMountType(String, String),
}

/// Namespace kinds the init child knows how to create or join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamespaceKind {
    Mount,
    Pid,
    Network,
    Ipc,
    Uts,
    User,
    Cgroup,
}

// `clone(2)` namespace flags (Linux ABI values, kept here so the planner
// stays compilable and testable on non-Linux hosts).
const CLONE_NEWNS: libc::c_int = 0x0002_0000;
const CLONE_NEWCGROUP: libc::c_int = 0x0200_0000;
const CLONE_NEWUTS: libc::c_int = 0x0400_0000;
const CLONE_NEWIPC: libc::c_int = 0x0800_0000;
const CLONE_NEWUSER: libc::c_int = 0x1000_0000;
const CLONE_NEWPID: libc::c_int = 0x2000_0000;
const CLONE_NEWNET: libc::c_int = 0x4000_0000;

impl NamespaceKind {
    /// `clone(2)` flag used when the namespace is created with `unshare`.
    pub fn clone_flag(self) -> libc::c_int {
        match self {
            NamespaceKind::Mount => CLONE_NEWNS,
            NamespaceKind::Pid => CLONE_NEWPID,
            NamespaceKind::Network => CLONE_NEWNET,
            NamespaceKind::Ipc => CLONE_NEWIPC,
            NamespaceKind::Uts => CLONE_NEWUTS,
            NamespaceKind::User => CLONE_NEWUSER,
            NamespaceKind::Cgroup => CLONE_NEWCGROUP,
        }
    }

    /// Name of the namespace symlink under `/proc/<pid>/ns/`.
    pub fn proc_ns_name(self) -> &'static str {
        match self {
            NamespaceKind::Mount => "mnt",
            NamespaceKind::Pid => "pid",
            NamespaceKind::Network => "net",
            NamespaceKind::Ipc => "ipc",
            NamespaceKind::Uts => "uts",
            NamespaceKind::User => "user",
            NamespaceKind::Cgroup => "cgroup",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespacePlan {
    pub kind: NamespaceKind,
    /// When set the child joins an existing namespace instead of unsharing.
    pub path: Option<PathBuf>,
}

/// One line of a uid_map/gid_map for user namespaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdMappingPlan {
    pub container_id: u32,
    pub host_id: u32,
    pub size: u32,
}

impl IdMappingPlan {
    pub fn render(&self) -> String {
        format!("{} {} {}", self.container_id, self.host_id, self.size)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPlan {
    pub uid: u32,
    pub gid: u32,
    pub additional_gids: Vec<u32>,
    pub umask: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPlan {
    pub bounding: Vec<String>,
    pub effective: Vec<String>,
    pub permitted: Vec<String>,
    pub inheritable: Vec<String>,
    pub ambient: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RlimitPlan {
    pub typ: String,
    pub soft: u64,
    pub hard: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessPlan {
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: PathBuf,
    pub user: UserPlan,
    pub capabilities: CapabilityPlan,
    pub rlimits: Vec<RlimitPlan>,
    pub no_new_privileges: bool,
}

/// One mount to apply inside the container's mount namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPlan {
    /// Absolute path inside the container namespace.
    pub destination: PathBuf,
    pub fs_type: String,
    pub source: Option<String>,
    pub options: Vec<String>,
    /// Mounts coming from the CreateTaskRequest must be applied before the
    /// spec mounts so layered filesystems exist before binds target them.
    pub from_request: bool,
}

/// Container IO as passed by containerd.
///
/// With `terminal` set, `stdout` carries the console socket path (the
/// containerd task v2 convention) and `stderr` is unused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoPlan {
    pub terminal: bool,
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

/// Pre-rendered cgroup v2 file contents for the limits the shim enforces.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CgroupPlan {
    /// Path relative to the unified cgroup mount, `:` separators already
    /// normalized to `/` (systemd-style `slice:prefix:id` becomes a path).
    pub path: Option<String>,
    pub pids_max: Option<String>,
    pub memory_max: Option<String>,
    pub cpu_max: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerPlan {
    pub id: String,
    pub bundle: PathBuf,
    pub rootfs: PathBuf,
    pub process: ProcessPlan,
    pub namespaces: Vec<NamespacePlan>,
    /// uid/gid mappings applied when a user namespace is unshared.
    pub uid_mappings: Vec<IdMappingPlan>,
    pub gid_mappings: Vec<IdMappingPlan>,
    pub hostname: Option<String>,
    /// Mounts from the OCI spec, ordered as they appear in config.json.
    pub mounts: Vec<MountPlan>,
    /// Rootfs mounts from the CreateTaskRequest (snapshotter output).
    pub rootfs_mounts: Vec<MountPlan>,
    pub root_readonly: bool,
    pub cgroup: Option<CgroupPlan>,
    pub io: IoPlan,
    pub annotations: HashMap<String, String>,
    /// Non-fatal gaps recorded while planning (surfaced in shim logs).
    pub warnings: Vec<String>,
}

/// Filesystem types the init child can mount without a warning.
const SUPPORTED_MOUNT_TYPES: &[&str] = &[
    "bind", "rbind", "proc", "tmpfs", "devpts", "sysfs", "cgroup", "cgroup2", "mqueue", "overlay",
];

/// Turn the OCI cwd (relative to the container root) into an absolute path
/// inside the container namespace.
pub fn normalize_cwd(cwd: &Path) -> PathBuf {
    if cwd.is_absolute() {
        normalize_absolute(cwd)
    } else {
        normalize_absolute(&Path::new("/").join(cwd))
    }
}

impl ContainerPlan {
    /// True when the init child must `unshare(CLONE_NEWUSER)` and write id
    /// mappings (as opposed to joining a user namespace by path).
    pub fn has_new_user_namespace(&self) -> bool {
        self.namespaces
            .iter()
            .any(|ns| ns.kind == NamespaceKind::User && ns.path.is_none())
    }

    /// True when a private UTS namespace (and thus hostname) is created.
    pub fn has_new_uts_namespace(&self) -> bool {
        self.namespaces
            .iter()
            .any(|ns| ns.kind == NamespaceKind::Uts && ns.path.is_none())
    }
}

/// Lexically clean an absolute path without touching the filesystem.
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// Convert `slice:prefix:id` systemd notation into a relative cgroup path.
pub fn normalize_cgroup_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    trimmed.replace(':', "/")
}

fn capability_names(caps: Option<&oci_spec::runtime::Capabilities>) -> Vec<String> {
    let Some(caps) = caps else {
        return Vec::new();
    };
    let mut names: Vec<String> = caps.iter().map(|cap| cap.to_string()).collect();
    names.sort();
    names
}

/// Render a cgroup v2 limit value; non-positive or absent limits mean "max".
fn render_limit(limit: Option<i64>) -> Option<String> {
    match limit {
        None => None,
        Some(value) if value <= 0 => Some("max".to_string()),
        Some(value) => Some(value.to_string()),
    }
}

fn plan_cgroup(spec: &Spec) -> Option<CgroupPlan> {
    let linux = spec.linux().as_ref()?;
    let resources = linux.resources().as_ref();
    let path = linux
        .cgroups_path()
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .map(|p| normalize_cgroup_path(&p));
    if resources.is_none() && path.is_none() {
        return None;
    }
    let mut plan = CgroupPlan {
        path,
        pids_max: None,
        memory_max: None,
        cpu_max: None,
    };
    if let Some(resources) = resources {
        if let Some(pids) = resources.pids().as_ref() {
            plan.pids_max = render_limit(Some(pids.limit()));
        }
        if let Some(memory) = resources.memory().as_ref() {
            plan.memory_max = render_limit(memory.limit());
        }
        if let Some(cpu) = resources.cpu().as_ref() {
            let period = cpu.period().unwrap_or(100_000);
            let quota = cpu.quota();
            let value = match quota {
                Some(q) if q > 0 => format!("{q} {period}"),
                _ => format!("max {period}"),
            };
            plan.cpu_max = Some(value);
        }
    }
    Some(plan)
}

fn plan_namespaces(
    spec: &Spec,
    warnings: &mut Vec<String>,
) -> Result<Vec<NamespacePlan>, PlanError> {
    let mut out = Vec::new();
    let namespaces = spec
        .linux()
        .as_ref()
        .and_then(|linux| linux.namespaces().clone())
        .unwrap_or_default();
    for namespace in namespaces {
        let kind = match namespace.typ() {
            LinuxNamespaceType::Mount => NamespaceKind::Mount,
            LinuxNamespaceType::Pid => NamespaceKind::Pid,
            LinuxNamespaceType::Network => NamespaceKind::Network,
            LinuxNamespaceType::Ipc => NamespaceKind::Ipc,
            LinuxNamespaceType::Uts => NamespaceKind::Uts,
            LinuxNamespaceType::User => NamespaceKind::User,
            LinuxNamespaceType::Cgroup => NamespaceKind::Cgroup,
            LinuxNamespaceType::Time => {
                warnings.push("time namespace ignored (M1 limitation)".to_string());
                continue;
            }
        };
        // Namespace paths (CRI pod containers point at the sandbox's
        // namespaces) are joined via setns in the internal parent; a pid
        // namespace join takes effect for the forked init child.
        out.push(NamespacePlan {
            kind,
            path: namespace.path().clone(),
        });
    }
    // The container always gets a private mount namespace for its rootfs.
    if !out.iter().any(|ns| ns.kind == NamespaceKind::Mount) {
        out.insert(
            0,
            NamespacePlan {
                kind: NamespaceKind::Mount,
                path: None,
            },
        );
    }
    Ok(out)
}

/// Build the full container plan.
///
/// `rootfs_mounts` are the converted `CreateTaskRequest.rootfs` entries;
/// conversion from protobuf stays on the Linux side.
/// Extract the process execution details from an OCI process section.
///
/// Shared by the init container (`config.json`) and exec processes (the
/// `ExecProcessRequest` spec).
pub fn process_plan_from(process: &oci_spec::runtime::Process) -> Result<ProcessPlan, PlanError> {
    let argv = process.args().clone().ok_or(PlanError::MissingArgs)?;
    if argv.is_empty() {
        return Err(PlanError::MissingArgs);
    }

    let user_spec = process.user();
    let user = UserPlan {
        uid: user_spec.uid(),
        gid: user_spec.gid(),
        additional_gids: user_spec.additional_gids().clone().unwrap_or_default(),
        umask: user_spec.umask(),
    };

    let capabilities = process
        .capabilities()
        .as_ref()
        .map(|caps| CapabilityPlan {
            bounding: capability_names(caps.bounding().as_ref()),
            effective: capability_names(caps.effective().as_ref()),
            permitted: capability_names(caps.permitted().as_ref()),
            inheritable: capability_names(caps.inheritable().as_ref()),
            ambient: capability_names(caps.ambient().as_ref()),
        })
        .unwrap_or_default();

    let rlimits = process
        .rlimits()
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|rlimit| RlimitPlan {
            typ: rlimit.typ().to_string(),
            soft: rlimit.soft(),
            hard: rlimit.hard(),
        })
        .collect();

    Ok(ProcessPlan {
        argv,
        env: process.env().clone().unwrap_or_default(),
        cwd: normalize_cwd(process.cwd()),
        user,
        capabilities,
        rlimits,
        no_new_privileges: process.no_new_privileges().unwrap_or(false),
    })
}

/// Plan for the pod sandbox holder (the pause-container replacement).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPlan {
    pub sandbox_id: String,
    pub hostname: Option<String>,
    /// Network namespace to join (the CRI pod netns); when unset the holder
    /// creates a fresh one.
    pub netns_path: Option<String>,
    /// True for shareProcessNamespace pods: the holder owns a pid namespace
    /// that member containers join.
    pub share_pid_namespace: bool,
}

/// Derive the sandbox holder plan from the CreateSandboxRequest fields.
///
/// The sandboxer contract carries no OCI spec — the sandbox shape is the
/// shim's own decision. The hostname falls back to the sandbox id prefix
/// (k8s pods surface the pod name via annotations).
pub fn build_sandbox_plan(
    sandbox_id: &str,
    netns_path: Option<&str>,
    annotations: &HashMap<String, String>,
) -> SandboxPlan {
    let hostname = annotations
        .get("io.kubernetes.pod.name")
        .cloned()
        .or_else(|| sandbox_id.get(..12).map(str::to_string));
    SandboxPlan {
        sandbox_id: sandbox_id.to_string(),
        hostname,
        netns_path: netns_path.map(str::to_string),
        share_pid_namespace: false,
    }
}

/// Resource shape for the libkrun microVM executor, derived from
/// `io.pvisor.vm.*` annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmConfig {
    pub cpus: u8,
    pub ram_mib: u32,
}

impl Default for VmConfig {
    fn default() -> Self {
        VmConfig {
            cpus: 2,
            ram_mib: 512,
        }
    }
}

impl ContainerPlan {
    /// True when the spec configures this namespace kind at all (created
    /// or joined); sandbox sharing only fills in the kinds it leaves out.
    pub fn has_namespace(&self, kind: NamespaceKind) -> bool {
        self.namespaces
            .iter()
            .any(|namespace| namespace.kind == kind)
    }

    /// True when the bundle asks for the libkrun VM executor via the
    /// `io.pvisor.executor` annotation (`host` keeps the default).
    pub fn wants_vm(&self) -> bool {
        self.annotations
            .get(format!("{ANNOTATION_PREFIX}executor").as_str())
            .is_some_and(|value| value == "vm")
    }

    /// VM shape from `io.pvisor.vm.cpus` / `io.pvisor.vm.memory-mib`.
    pub fn vm_config(&self) -> VmConfig {
        let mut config = VmConfig::default();
        if let Some(cpus) = self
            .annotations
            .get(format!("{ANNOTATION_PREFIX}vm.cpus").as_str())
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|cpus| *cpus > 0)
        {
            config.cpus = cpus;
        }
        if let Some(ram) = self
            .annotations
            .get(format!("{ANNOTATION_PREFIX}vm.memory-mib").as_str())
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|ram| *ram > 0)
        {
            config.ram_mib = ram;
        }
        config
    }
}

/// True unless `io.pvisor.vm.agent=off`: VMs boot the guest agent so exec
/// works (the shim binary is copied into the rootfs at boot).
pub fn vm_agent_enabled(annotations: &HashMap<String, String>) -> bool {
    annotations
        .get(format!("{ANNOTATION_PREFIX}vm.agent").as_str())
        .map(|value| value != "off")
        .unwrap_or(true)
}

/// Build the same launch contract used by pVisor's VM executor.
pub fn validate_vm_process(
    process: &oci_spec::runtime::Process,
    exec: bool,
) -> Result<(), PlanError> {
    let user = process.user();
    if user.uid() != 0
        || user.gid() != 0
        || user
            .additional_gids()
            .as_ref()
            .is_some_and(|groups| !groups.is_empty())
        || user.umask().is_some()
    {
        return Err(PlanError::UnsupportedVmProcess("uid/gid/groups/umask"));
    }
    // Even explicitly empty capability sets are a restriction we cannot install.
    if process.capabilities().is_some() {
        return Err(PlanError::UnsupportedVmProcess("capabilities"));
    }
    if process.no_new_privileges().unwrap_or(false) {
        return Err(PlanError::UnsupportedVmProcess("noNewPrivileges"));
    }
    if exec
        && process
            .rlimits()
            .as_ref()
            .is_some_and(|limits| !limits.is_empty())
    {
        return Err(PlanError::UnsupportedVmProcess("exec rlimits"));
    }
    Ok(())
}

pub fn guest_config(
    process: &ProcessPlan,
    agent: bool,
) -> anyhow::Result<pvisor_guest::GuestConfig> {
    anyhow::ensure!(
        process.user == UserPlan::default()
            && process.capabilities == CapabilityPlan::default()
            && !process.no_new_privileges,
        "VM process security constraints are unsupported; refusing to discard them"
    );
    let env = process
        .env
        .iter()
        .map(|entry| {
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid environment entry {entry:?}"))?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect::<anyhow::Result<_>>()?;
    let config = pvisor_guest::GuestConfig {
        argv: process.argv.clone(),
        env,
        cwd: process.cwd.clone(),
        limits: process
            .rlimits
            .iter()
            .map(|limit| (limit.typ.clone(), (limit.soft, limit.hard)))
            .collect(),
        agent: agent.then(|| {
            vec![
                crate::agent::AGENT_GUEST_PATH.into(),
                crate::agent::AGENT_ARG.into(),
            ]
        }),
        ..Default::default()
    };
    config.command()?;
    Ok(config)
}

/// Plan for one exec process inside a running container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecPlan {
    pub container_id: String,
    pub exec_id: String,
    /// Init process pid; the exec child joins its namespaces.
    pub init_pid: u32,
    pub process: ProcessPlan,
    pub io: IoPlan,
}

/// Build an exec plan from the OCI process spec of the exec request.
pub fn build_exec_plan(
    process: &oci_spec::runtime::Process,
    container_id: &str,
    exec_id: &str,
    init_pid: u32,
    io: IoPlan,
) -> Result<ExecPlan, PlanError> {
    Ok(ExecPlan {
        container_id: container_id.to_string(),
        exec_id: exec_id.to_string(),
        init_pid,
        process: process_plan_from(process)?,
        io,
    })
}

pub fn build_plan(
    spec: &Spec,
    id: &str,
    bundle: &Path,
    rootfs_mounts: Vec<MountPlan>,
    io: IoPlan,
) -> Result<ContainerPlan, PlanError> {
    let process = spec.process().as_ref().ok_or(PlanError::MissingProcess)?;
    if spec
        .annotations()
        .as_ref()
        .and_then(|annotations| annotations.get("io.pvisor.executor"))
        .is_some_and(|executor| executor == "vm")
    {
        validate_vm_process(process, false)?;
    }
    let process_plan = process_plan_from(process)?;

    let mut warnings = Vec::new();
    if spec
        .linux()
        .as_ref()
        .and_then(|linux| linux.seccomp().as_ref())
        .is_some()
    {
        warnings.push("seccomp profile ignored (M1 limitation)".to_string());
    }
    if spec.hooks().is_some() {
        warnings.push("OCI hooks ignored (M1 limitation)".to_string());
    }
    if spec
        .linux()
        .as_ref()
        .and_then(|linux| linux.masked_paths().clone())
        .is_some_and(|masked| !masked.is_empty())
    {
        warnings.push("maskedPaths ignored (M1 limitation)".to_string());
    }

    let namespaces = plan_namespaces(spec, &mut warnings)?;

    let map_mappings = |mappings: Option<&Vec<oci_spec::runtime::LinuxIdMapping>>| {
        mappings
            .map(|entries| {
                entries
                    .iter()
                    .map(|m| IdMappingPlan {
                        container_id: m.container_id(),
                        host_id: m.host_id(),
                        size: m.size(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let linux = spec.linux().as_ref();
    let uid_mappings = map_mappings(linux.and_then(|l| l.uid_mappings().as_ref()));
    let gid_mappings = map_mappings(linux.and_then(|l| l.gid_mappings().as_ref()));

    let mut mounts = Vec::new();
    for mount in spec.mounts().clone().unwrap_or_default() {
        let fs_type = mount.typ().clone().unwrap_or_else(|| "bind".to_string());
        if !SUPPORTED_MOUNT_TYPES.contains(&fs_type.as_str()) {
            return Err(PlanError::UnsupportedMountType(
                fs_type,
                mount.destination().display().to_string(),
            ));
        }
        mounts.push(MountPlan {
            destination: normalize_absolute(&Path::new("/").join(mount.destination())),
            fs_type,
            source: mount
                .source()
                .as_ref()
                .map(|s| s.to_string_lossy().to_string()),
            options: mount.options().clone().unwrap_or_default(),
            from_request: false,
        });
    }

    let rootfs = spec
        .root()
        .as_ref()
        .map(|root| bundle.join(root.path()))
        .unwrap_or_else(|| bundle.join("rootfs"));

    let annotations = spec.annotations().clone().unwrap_or_default();

    Ok(ContainerPlan {
        id: id.to_string(),
        bundle: bundle.to_path_buf(),
        rootfs,
        process: process_plan,
        namespaces,
        uid_mappings,
        gid_mappings,
        hostname: spec.hostname().clone(),
        mounts,
        rootfs_mounts,
        root_readonly: spec
            .root()
            .as_ref()
            .and_then(|root| root.readonly())
            .unwrap_or(false),
        cgroup: plan_cgroup(spec),
        io,
        annotations,
        warnings,
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn vm_constraints_fail_before_unsupported_security_is_discarded() {
        let mut base = serde_json::to_value(minimal_spec()).unwrap();
        base["annotations"] = serde_json::json!({"io.pvisor.executor":"vm"});
        for (field, value) in [
            ("user", serde_json::json!({"uid":1000,"gid":1000})),
            ("capabilities", serde_json::json!({})),
            ("noNewPrivileges", serde_json::json!(true)),
        ] {
            let mut value_spec = base.clone();
            value_spec["process"][field] = value;
            let spec: Spec = serde_json::from_value(value_spec).unwrap();
            assert!(
                build_plan(&spec, "vm", Path::new("/bundle"), vec![], IoPlan::default()).is_err()
            );
            assert!(validate_vm_process(spec.process().as_ref().unwrap(), true).is_err());
        }
        let mut exec = base;
        exec["process"]["rlimits"] =
            serde_json::json!([{"type":"RLIMIT_NOFILE","soft":32,"hard":64}]);
        let spec: Spec = serde_json::from_value(exec).unwrap();
        assert!(validate_vm_process(spec.process().as_ref().unwrap(), false).is_ok());
        assert!(validate_vm_process(spec.process().as_ref().unwrap(), true).is_err());
    }
    use super::*;

    fn spec_from_json(json: &str) -> Spec {
        serde_json::from_str(json).expect("parse spec")
    }

    fn minimal_spec() -> Spec {
        spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"root":{"path":"rootfs"}}"#,
        )
    }

    #[test]
    fn cwd_relative_to_root_is_normalized_absolute() {
        assert_eq!(normalize_cwd(Path::new("/tmp/x")), PathBuf::from("/tmp/x"));
        assert_eq!(normalize_cwd(Path::new("tmp/x")), PathBuf::from("/tmp/x"));
        assert_eq!(
            normalize_cwd(Path::new("/tmp/../var/./log")),
            PathBuf::from("/var/log")
        );
        assert_eq!(normalize_cwd(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn systemd_cgroup_paths_become_slash_separated() {
        assert_eq!(
            normalize_cgroup_path("kubepods.slice:cri-containerd:test-id"),
            "kubepods.slice/cri-containerd/test-id"
        );
        assert_eq!(
            normalize_cgroup_path("/system.slice/foo.service"),
            "system.slice/foo.service"
        );
    }

    #[test]
    fn minimal_plan_has_private_mount_namespace() {
        let plan = build_plan(
            &minimal_spec(),
            "c1",
            Path::new("/bundle"),
            vec![],
            IoPlan::default(),
        )
        .expect("plan");
        assert_eq!(plan.process.argv, vec!["/bin/sh".to_string()]);
        assert_eq!(plan.rootfs, PathBuf::from("/bundle/rootfs"));
        assert!(
            plan.namespaces
                .iter()
                .any(|ns| ns.kind == NamespaceKind::Mount && ns.path.is_none())
        );
        assert_eq!(plan.cgroup, None);
    }

    #[test]
    fn missing_process_or_args_is_rejected() {
        let no_process = spec_from_json(r#"{"ociVersion":"1.0.2"}"#);
        assert!(matches!(
            build_plan(
                &no_process,
                "c1",
                Path::new("/b"),
                vec![],
                IoPlan::default()
            ),
            Err(PlanError::MissingProcess)
        ));
        let no_args = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"cwd":"/","user":{"uid":0,"gid":0}}}"#,
        );
        assert!(matches!(
            build_plan(&no_args, "c1", Path::new("/b"), vec![], IoPlan::default()),
            Err(PlanError::MissingArgs)
        ));
    }

    #[test]
    fn namespace_paths_join_the_sandbox() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"linux":{"namespaces":[{"type":"pid","path":"/proc/123/ns/pid"},{"type":"mount","path":"/proc/123/ns/mnt"}]}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        let pid = plan
            .namespaces
            .iter()
            .find(|ns| ns.kind == NamespaceKind::Pid)
            .expect("pid ns");
        assert_eq!(pid.path.as_deref(), Some(Path::new("/proc/123/ns/pid")));
        let mount = plan
            .namespaces
            .iter()
            .find(|ns| ns.kind == NamespaceKind::Mount)
            .expect("mount ns");
        assert_eq!(mount.path.as_deref(), Some(Path::new("/proc/123/ns/mnt")));
    }

    #[test]
    fn namespaces_map_with_stable_join_semantics() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"linux":{"namespaces":[{"type":"pid"},{"type":"network","path":"/var/run/netns/n1"},{"type":"time"}]}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        let kinds: Vec<_> = plan
            .namespaces
            .iter()
            .map(|ns| (ns.kind, ns.path.is_some()))
            .collect();
        // mount namespace inserted first, then pid, then joined network.
        assert_eq!(kinds[0], (NamespaceKind::Mount, false));
        assert_eq!(kinds[1], (NamespaceKind::Pid, false));
        assert_eq!(kinds[2], (NamespaceKind::Network, true));
        assert!(plan.warnings.iter().any(|w| w.contains("time namespace")));
    }

    #[test]
    fn cgroup_limits_render_cgroupv2_values() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"linux":{"cgroupsPath":"burstable.slice:pod123:abc","resources":{"pids":{"limit":128},"memory":{"limit":536870912},"cpu":{"quota":500000,"period":100000}}}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        let cgroup = plan.cgroup.expect("cgroup planned");
        assert_eq!(cgroup.path.as_deref(), Some("burstable.slice/pod123/abc"));
        assert_eq!(cgroup.pids_max.as_deref(), Some("128"));
        assert_eq!(cgroup.memory_max.as_deref(), Some("536870912"));
        assert_eq!(cgroup.cpu_max.as_deref(), Some("500000 100000"));
    }

    #[test]
    fn unlimited_cpu_renders_max() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"linux":{"resources":{"cpu":{"quota":-1}}}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        assert_eq!(
            plan.cgroup.and_then(|c| c.cpu_max),
            Some("max 100000".to_string())
        );
    }

    #[test]
    fn unsupported_mounts_are_rejected() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"mounts":[{"destination":"/data","type":"ceph","source":"mon1:/"},{"destination":"/proc","type":"proc"}],"linux":{"seccomp":{"defaultAction":"SCMP_ACT_ERRNO"}}}"#,
        );
        let error =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).unwrap_err();
        assert!(error.to_string().contains("unsupported mount type ceph"));
    }

    #[test]
    fn sandbox_plan_derives_holder_semantics() {
        let mut annotations = HashMap::new();
        annotations.insert("io.kubernetes.pod.name".to_string(), "my-pod".to_string());
        let plan = build_sandbox_plan("sbx1", Some("/var/run/netns/n1"), &annotations);
        assert_eq!(plan.hostname.as_deref(), Some("my-pod"));
        assert_eq!(plan.netns_path.as_deref(), Some("/var/run/netns/n1"));
        assert!(!plan.share_pid_namespace);

        // Without a pod annotation the hostname falls back to the id prefix.
        let fallback = build_sandbox_plan("abcdef1234567890", None, &HashMap::new());
        assert_eq!(fallback.hostname.as_deref(), Some("abcdef123456"));
        assert!(fallback.netns_path.is_none());
    }

    #[test]
    fn has_namespace_reflects_spec_configuration() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/","user":{"uid":0,"gid":0}},"linux":{"namespaces":[{"type":"pid"},{"type":"network","path":"/var/run/netns/n1"}]}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        assert!(plan.has_namespace(NamespaceKind::Pid));
        assert!(plan.has_namespace(NamespaceKind::Network));
        assert!(!plan.has_namespace(NamespaceKind::Uts));
        assert!(!plan.has_namespace(NamespaceKind::Ipc));
        // The planner always inserts a private mount namespace.
        assert!(plan.has_namespace(NamespaceKind::Mount));
    }

    #[test]
    fn vm_executor_is_annotation_driven() {
        let base = minimal_spec();
        let plan =
            build_plan(&base, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        assert!(!plan.wants_vm());
        assert_eq!(plan.vm_config(), VmConfig::default());

        let mut vm_spec = minimal_spec();
        vm_spec
            .annotations_mut()
            .get_or_insert_with(Default::default)
            .insert("io.pvisor.executor".to_string(), "vm".to_string());
        vm_spec
            .annotations_mut()
            .get_or_insert_with(Default::default)
            .insert("io.pvisor.vm.cpus".to_string(), "4".to_string());
        vm_spec
            .annotations_mut()
            .get_or_insert_with(Default::default)
            .insert("io.pvisor.vm.memory-mib".to_string(), "1024".to_string());
        let plan =
            build_plan(&vm_spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        assert!(plan.wants_vm());
        assert_eq!(
            plan.vm_config(),
            VmConfig {
                cpus: 4,
                ram_mib: 1024
            }
        );
    }

    #[test]
    fn guest_config_preserves_arguments_environment_and_agent() {
        let process = ProcessPlan {
            argv: vec!["/bin/sh".into(), "-c".into(), "echo 'hi there'".into()],
            env: vec!["GREETING=hello world".into()],
            cwd: PathBuf::from("/work dir"),
            user: UserPlan::default(),
            capabilities: CapabilityPlan::default(),
            rlimits: vec![RlimitPlan {
                typ: "RLIMIT_NOFILE".into(),
                soft: 32,
                hard: 64,
            }],
            no_new_privileges: false,
        };
        let config = guest_config(&process, true).unwrap();
        assert_eq!(config.argv, process.argv);
        assert_eq!(config.env["GREETING"], "hello world");
        assert_eq!(config.limits["RLIMIT_NOFILE"], (32, 64));
        assert_eq!(
            config.agent.unwrap(),
            [crate::agent::AGENT_GUEST_PATH, crate::agent::AGENT_ARG]
        );
        assert!(guest_config(&process, false).unwrap().agent.is_none());
    }

    #[test]
    fn vm_agent_annotation_controls_the_agent() {
        let mut annotations = HashMap::new();
        assert!(vm_agent_enabled(&annotations));
        annotations.insert("io.pvisor.vm.agent".to_string(), "off".to_string());
        assert!(!vm_agent_enabled(&annotations));
        annotations.insert("io.pvisor.vm.agent".to_string(), "on".to_string());
        assert!(vm_agent_enabled(&annotations));
    }

    #[test]
    fn exec_plan_reuses_the_process_extraction() {
        let process: oci_spec::runtime::Process = serde_json::from_str(
            r#"{"args":["/bin/ls","-l"],"cwd":"/tmp","user":{"uid":0,"gid":0},"env":["FOO=bar"]}"#,
        )
        .expect("parse process");
        let plan = build_exec_plan(
            &process,
            "c1",
            "e1",
            4242,
            IoPlan {
                terminal: false,
                stdin: Some("/bundle/stdin".to_string()),
                stdout: Some("/bundle/stdout".to_string()),
                stderr: Some("/bundle/stderr".to_string()),
            },
        )
        .expect("exec plan");
        assert_eq!(plan.init_pid, 4242);
        assert_eq!(
            plan.process.argv,
            vec!["/bin/ls".to_string(), "-l".to_string()]
        );
        assert_eq!(plan.process.cwd, PathBuf::from("/tmp"));
        assert_eq!(plan.process.env, vec!["FOO=bar".to_string()]);

        let no_args = serde_json::from_str::<oci_spec::runtime::Process>(
            r#"{"cwd":"/","user":{"uid":0,"gid":0}}"#,
        )
        .expect("parse process");
        assert!(matches!(
            build_exec_plan(&no_args, "c1", "e1", 1, IoPlan::default()),
            Err(PlanError::MissingArgs)
        ));
    }

    #[test]
    fn request_mounts_are_kept_separate_from_spec_mounts() {
        let request_mounts = vec![MountPlan {
            destination: PathBuf::from("/"),
            fs_type: "overlay".to_string(),
            source: Some("overlay".to_string()),
            options: vec!["lowerdir=/snaps/l1".to_string()],
            from_request: true,
        }];
        let plan = build_plan(
            &minimal_spec(),
            "c1",
            Path::new("/bundle"),
            request_mounts,
            IoPlan::default(),
        )
        .expect("plan");
        assert!(plan.rootfs_mounts[0].from_request);
        assert!(!plan.mounts.iter().any(|m| m.from_request));
    }

    #[test]
    fn user_and_capabilities_carry_through() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","process":{"args":["/bin/sh"],"cwd":"/w","user":{"uid":1000,"gid":2000,"additionalGids":[2000,999]},"capabilities":{"bounding":["CAP_CHOWN","CAP_NET_BIND_SERVICE"],"effective":["CAP_CHOWN"]},"rlimits":[{"type":"RLIMIT_NOFILE","soft":1024,"hard":2048}],"noNewPrivileges":true},"root":{"path":"rootfs","readonly":true}}"#,
        );
        let plan =
            build_plan(&spec, "c1", Path::new("/b"), vec![], IoPlan::default()).expect("plan");
        assert_eq!(plan.process.user.uid, 1000);
        assert_eq!(plan.process.user.additional_gids, vec![2000, 999]);
        assert_eq!(plan.process.cwd, PathBuf::from("/w"));
        assert!(plan.process.no_new_privileges);
        assert!(plan.root_readonly);
        // Capability names arrive via Display ("CAP_" prefix is stripped by
        // oci-spec); caps::from_names re-normalizes them.
        assert_eq!(plan.process.capabilities.bounding.len(), 2);
        assert_eq!(plan.process.rlimits.len(), 1);
        assert_eq!(plan.process.rlimits[0].typ, "RLIMIT_NOFILE");
    }
}

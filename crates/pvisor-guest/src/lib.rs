//! Launch contract shared by the host executors and the Linux guest.
pub mod control;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const CONFIG_PATH: &str = "/.pvisor-guest.json";

/// Private guest RAM filesystem created before launching the workload.
/// The path must be a fresh absolute directory with an existing parent.
/// `size_bytes` caps filesystem capacity inside the existing guest RAM budget;
/// it does not reserve or add RAM. Cold launches start empty. VM RAM snapshots
/// preserve its contents; it does not belong to the host workspace stage.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryFilesystem {
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestConfig {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// Linux RLIMIT names, with soft and hard limits in native units.
    #[serde(default)]
    pub limits: BTreeMap<String, (u64, u64)>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub agent: Option<Vec<String>>,
    /// Daemons started alongside the workload (see [`SidecarProcess`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<SidecarProcess>,
    /// AF_VSOCK → guest-TCP relays (see [`PortBridge`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<PortBridge>,
    /// Absolute mount point of the read-only shared payloads filesystem
    /// (virtio-fs tag `pvisor-payloads`); staged payload symlinks resolve
    /// here. One host copy serves every guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payloads_dir: Option<PathBuf>,
    /// Host directories shared into the guest as virtio-fs mounts (pod
    /// volume bind mounts the VM tier re-expresses as shares; see
    /// [`GuestShare`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<GuestShare>,
    /// Named virtio-console ports required for stdin, stdout and stderr.
    /// The runner fills this from its actual descriptors before booting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdio_ports: Option<[bool; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary_filesystem: Option<TemporaryFilesystem>,
    /// Optional process setup. Omission inherits the bootstrap identity, session
    /// and console descriptors; all workloads use the same process supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessConfig>,
    /// Additional virtio-fs shares installed before the workload starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    /// Enable the host-only vsock command adapter; false starts no listener.
    #[serde(default)]
    pub control: bool,
}

/// Process setup shared by the initial workload and subsequent exec requests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessConfig {
    /// Guest UID/name and optional group. None preserves the bootstrap identity;
    /// Some(empty) selects root. Missing HOME is resolved from guest passwd only.
    pub user: Option<String>,
    /// Linux capability bounding set to retain. None leaves it unchanged.
    /// Values must be in 0..64; dropping capabilities precedes changing identity.
    pub capabilities: Option<Vec<u32>>,
    /// Start a separate session/process group, required for PTYs and control.
    pub new_session: bool,
    /// Initial stdin ownership; omission retains the bootstrap descriptor.
    pub stdin: StdinMode,
    /// Optional PTY dimensions, rows then columns, both nonzero. A PTY combines
    /// output streams and requires a new session.
    pub tty: Option<[u16; 2]>,
    /// Create the working directory before spawning; false requires it to exist.
    pub create_workdir: bool,
}

/// Ownership of the initial process's input descriptor.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    /// Inherit the bootstrap console, retaining ordinary Job terminal behavior.
    #[default]
    Inherit,
    /// Open /dev/null for noninteractive workloads.
    Closed,
    /// Retain a pipe or PTY input writer for one control-channel attachment.
    Pipe,
}

/// A host-declared share; its tag identifies a VMM filesystem, never a host path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// VMM filesystem tag.
    pub tag: String,
    /// Absolute guest destination without parent traversal. The host must also
    /// reject overlap with guest system/control paths before attaching the share.
    pub target: PathBuf,
    /// Reject guest mutations at the mount boundary.
    pub read_only: bool,
    /// Populate an empty share from its image destination before mounting it.
    /// Only directories, regular files and symlinks are supported.
    pub initialize: bool,
}

/// A daemon the supervisor starts alongside the workload. Sidecars are
/// advisory by contract: they run with the workload environment overlaid by
/// their own, stdio detached (stdin/stdout null, stderr appended to `log`
/// inside the guest root, host-visible through virtio-fs), and their spawn
/// failure or exit never affects the workload or the reported guest exit
/// code. argv[0] addresses a file staged into the rootfs by the host
/// executor; the supervisor performs no lookup or validation of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarProcess {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Absolute guest path receiving the sidecar's stderr log.
    pub log: PathBuf,
}

/// Relay one host connection end to end: listen on the guest AF_VSOCK
/// `vsock_port` (the host executor tunnels a unix socket into it) and
/// forward every accepted connection to guest `127.0.0.1:tcp_port`, where
/// the bridged service listens. The relay is advisory: a failed listener or
/// connection is reported on the console and never affects the workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortBridge {
    pub vsock_port: u32,
    pub tcp_port: u16,
}

/// One host directory shared into the guest at `path`. Read-only shares
/// are physically immutable on the host side (DAX-capable); the guest
/// mounts them `ro,dax=never` — ordinary reads through the single host
/// page-cache copy, no window-size coupling to dataset size. Read-write
/// shares are plain writable passthrough.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestShare {
    /// Virtio-fs tag; unique within one guest.
    pub tag: String,
    /// Absolute guest mount point (never `/`).
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub address: [u8; 4],
    pub gateway: [u8; 4],
}

impl GuestConfig {
    pub fn command(&self) -> io::Result<Command> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err(invalid("guest argv needs a program"));
        }
        if !self.cwd.is_absolute() || self.workspace.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(invalid("guest paths must be absolute"));
        }
        for path in std::iter::once(&self.cwd).chain(self.workspace.iter()) {
            CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest paths contain NUL"))?;
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|argv| argv.first().is_none_or(|p| p.is_empty()))
        {
            return Err(invalid("guest agent argv needs a program"));
        }
        for sidecar in &self.sidecars {
            if sidecar.argv.first().is_none_or(|p| p.is_empty()) {
                return Err(invalid("guest sidecar argv needs a program"));
            }
            if !sidecar.log.is_absolute() {
                return Err(invalid("guest sidecar log must be absolute"));
            }
            CString::new(sidecar.log.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest sidecar log contains NUL"))?;
        }
        for word in self
            .argv
            .iter()
            .chain(self.env.values())
            .chain(self.agent.iter().flatten())
            .chain(self.sidecars.iter().flat_map(|sidecar| sidecar.argv.iter()))
            .chain(
                self.sidecars
                    .iter()
                    .flat_map(|sidecar| sidecar.env.values()),
            )
        {
            CString::new(word.as_str()).map_err(|_| invalid("guest arguments contain NUL"))?;
        }
        if self
            .env
            .keys()
            .chain(self.sidecars.iter().flat_map(|sidecar| sidecar.env.keys()))
            .any(|key| key.is_empty() || key.contains(['=', '\0']))
        {
            return Err(invalid("invalid guest environment name"));
        }
        if self.limits.values().any(|(soft, hard)| soft > hard) {
            return Err(invalid("guest soft limit exceeds hard limit"));
        }
        if let Some(process) = &self.process {
            if let Some(user) = &process.user {
                CString::new(user.as_str()).map_err(|_| invalid("guest user contains NUL"))?;
            }
            if process
                .capabilities
                .as_ref()
                .is_some_and(|caps| caps.iter().any(|cap| *cap >= 64))
            {
                return Err(invalid("invalid capability bounding set"));
            }
            if process
                .tty
                .is_some_and(|size| size.contains(&0) || !process.new_session)
            {
                return Err(invalid("guest PTY requires dimensions and a new session"));
            }
        }
        if self.control
            && !self
                .process
                .as_ref()
                .is_some_and(|process| process.new_session)
        {
            return Err(invalid(
                "guest control requires a separate workload session",
            ));
        }
        for mount in &self.mounts {
            if !mount.target.is_absolute()
                || mount.target.parent().is_none()
                || mount.target.components().any(|component| {
                    !matches!(
                        component,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
            {
                return Err(invalid("invalid guest mount target"));
            }
            CString::new(mount.target.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest mount target contains NUL"))?;
            CString::new(mount.tag.as_str())
                .map_err(|_| invalid("guest mount tag contains NUL"))?;
        }
        if let Some(scratch) = &self.temporary_filesystem {
            if scratch.size_bytes == 0
                || !scratch.path.is_absolute()
                || scratch.path.parent().is_none()
                || scratch.path.components().any(|component| {
                    !matches!(
                        component,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
                || self.workspace.as_ref().is_some_and(|workspace| {
                    scratch.path.starts_with(workspace) || workspace.starts_with(&scratch.path)
                })
            {
                return Err(invalid("invalid guest temporary filesystem"));
            }
            CString::new(scratch.path.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest temporary filesystem path contains NUL"))?;
        }
        if self
            .bridges
            .iter()
            .any(|bridge| bridge.vsock_port == 0 || bridge.tcp_port == 0)
            || self
                .bridges
                .iter()
                .map(|bridge| bridge.vsock_port)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.bridges.len()
        {
            return Err(invalid("invalid guest port bridge"));
        }
        if self
            .payloads_dir
            .as_ref()
            .is_some_and(|dir| !dir.is_absolute())
        {
            return Err(invalid("guest payloads mount must be absolute"));
        }
        if self.shares.iter().any(|share| {
            share.tag.is_empty()
                || share.tag.contains('\0')
                || !share.path.is_absolute()
                || share.path == std::path::Path::new("/")
                || CString::new(share.path.as_os_str().as_encoded_bytes()).is_err()
        }) || self
            .shares
            .iter()
            .map(|share| share.tag.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != self.shares.len()
        {
            return Err(invalid("invalid guest share"));
        }
        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.cwd);
        Ok(command)
    }

    /// Build the launch command of one sidecar: its argv with the workload
    /// environment overlaid by its own (sidecar entries win on conflicts).
    /// Stdio wiring is the supervisor's business (see [`SidecarProcess`]).
    pub fn sidecar_command(&self, sidecar: &SidecarProcess) -> io::Result<Command> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let program = sidecar
            .argv
            .first()
            .ok_or_else(|| invalid("guest sidecar argv needs a program"))?;
        let mut command = Command::new(program);
        command
            .args(&sidecar.argv[1..])
            .env_clear()
            .envs(&self.env)
            .envs(&sidecar.env);
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_process_options_preserve_old_defaults_and_validate_before_launch() {
        let mut config: GuestConfig =
            serde_json::from_str(r#"{"argv":["/bin/true"],"env":{},"cwd":"/"}"#).unwrap();
        assert!(config.process.is_none());
        assert!(!config.control);
        assert!(config.mounts.is_empty());
        assert!(config.command().is_ok());
        config.control = true;
        assert!(config.command().is_err());
        config.process = Some(ProcessConfig {
            new_session: true,
            ..Default::default()
        });
        assert!(config.command().is_ok());
        config.process.as_mut().unwrap().tty = Some([0, 80]);
        assert!(config.command().is_err());
        config.process.as_mut().unwrap().tty = Some([24, 80]);
        config.process.as_mut().unwrap().capabilities = Some(vec![64]);
        assert!(config.command().is_err());
        config.process.as_mut().unwrap().capabilities = Some(vec![0, 1]);
        config.mounts.push(Mount {
            tag: "data".into(),
            target: "/data".into(),
            read_only: true,
            initialize: false,
        });
        assert!(config.command().is_ok());
        for path in ["/", "relative", "/data/../proc", "/bad\0path"] {
            config.mounts[0].target = path.into();
            assert!(config.command().is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn temporary_filesystem_contract_is_optional_bounded_and_separate_from_workspace() {
        let mut config: GuestConfig = serde_json::from_str(
            r#"{"argv":["/bin/true"],"env":{},"cwd":"/","workspace":"/work"}"#,
        )
        .unwrap();
        assert!(config.temporary_filesystem.is_none());
        assert!(
            !serde_json::to_value(&config)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("temporary_filesystem")
        );
        config.temporary_filesystem = Some(TemporaryFilesystem {
            path: "/.pvisor-tmp-test".into(),
            size_bytes: 64 * 1024 * 1024,
        });
        assert!(config.command().is_ok());
        let encoded = serde_json::to_vec(&config).unwrap();
        let decoded: GuestConfig = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded.temporary_filesystem.unwrap().size_bytes,
            64 * 1024 * 1024
        );
        for path in [
            "/",
            "relative",
            "/work/tmp",
            "/work",
            "/.scratch/../work",
            "/bad\0path",
        ] {
            config.temporary_filesystem.as_mut().unwrap().path = path.into();
            assert!(config.command().is_err(), "accepted {path:?}");
        }
        let scratch = config.temporary_filesystem.as_mut().unwrap();
        scratch.path = "/.pvisor-tmp-test".into();
        scratch.size_bytes = 0;
        assert!(config.command().is_err());
    }

    #[test]
    fn sidecars_validate_optionally_and_overlay_workload_environment() {
        let mut config: GuestConfig =
            serde_json::from_str(r#"{"argv":["/bin/true"],"env":{},"cwd":"/"}"#).unwrap();
        assert!(
            !serde_json::to_value(&config)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("sidecars")
        );
        let sidecar = SidecarProcess {
            argv: vec!["/.payload".into(), "--serve".into()],
            env: BTreeMap::from([("PAYLOAD_PORT".into(), "44772".into())]),
            log: "/.payload.log".into(),
        };
        config.sidecars = vec![sidecar.clone()];
        // Validation rejects sidecars with no program, relative logs, NUL
        // words and invalid environment names, like every other field.
        assert!(config.command().is_ok());
        let encoded = serde_json::to_vec(&config).unwrap();
        let decoded: GuestConfig = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.sidecars, vec![sidecar]);
        let mut bad = config.clone();
        bad.sidecars[0].argv.clear();
        assert!(bad.command().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].log = "relative.log".into();
        assert!(bad.command().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].argv.push("bad\0word".into());
        assert!(bad.command().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].env.insert("A=B".into(), "1".into());
        assert!(bad.command().is_err());
        let mut foreign: serde_json::Value = serde_json::to_value(&config).unwrap();
        foreign["sidecars"][0]["bogus"] = serde_json::json!(1);
        assert!(serde_json::from_value::<GuestConfig>(foreign).is_err());

        // The sidecar command inherits the workload environment with its
        // own entries winning on conflicts.
        config.env.insert("SHARED".into(), "workload".into());
        config.env.insert("PAYLOAD_PORT".into(), "1".into());
        let command = config.sidecar_command(&config.sidecars[0]).unwrap();
        let envs: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert_eq!(envs["SHARED"].as_deref(), Some("workload"));
        assert_eq!(envs["PAYLOAD_PORT"].as_deref(), Some("44772"));
    }

    #[test]
    fn launch_contract_preserves_values_and_rejects_invalid_input() {
        let config = GuestConfig {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "test -z \"${HOME+x}\" || exit 41; printf '%s' \"$COMPLEX\"; exit 7".into(),
            ],
            env: BTreeMap::from([("COMPLEX".into(), "space ' quote \"\nnewline".into())]),
            cwd: std::env::temp_dir(),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let mut decoded: GuestConfig = serde_json::from_slice(&bytes).unwrap();
        let output = decoded.command().unwrap().output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            config.env["COMPLEX"]
        );
        decoded.argv.push("bad\0argument".into());
        assert!(decoded.command().is_err());
        decoded.argv.clear();
        assert!(decoded.command().is_err());
    }
}

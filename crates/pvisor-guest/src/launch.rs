//! Launch contract shared by the host executors and the Linux guest.
use crate::api::{GuestConfig, GuestValidation, StdinMode};
#[cfg(test)]
use crate::api::{GuestShare, Mount, ProcessConfig, TemporaryFilesystem};
use std::ffi::CString;
use std::io;
use std::path::Path;

// Launch paths belong to the Linux guest, not the host validating the message.
fn guest_absolute(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().starts_with(b"/")
}

fn guest_components(path: &Path) -> impl Iterator<Item = &[u8]> {
    path.as_os_str()
        .as_encoded_bytes()
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
}

fn guest_mount_target(path: &Path) -> bool {
    guest_absolute(path)
        && guest_components(path).next().is_some()
        && !guest_components(path).any(|part| part == b"..")
}

fn guest_path_starts_with(path: &Path, prefix: &Path) -> bool {
    let mut parts = guest_components(path);
    guest_components(prefix).all(|part| parts.next() == Some(part))
}

impl GuestValidation for GuestConfig {
    fn validate(&self) -> io::Result<()> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        if self.control_ready && !self.control {
            return Err(invalid(
                "control readiness notification requires the vsock command adapter",
            ));
        }
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err(invalid("guest argv needs a program"));
        }
        if !guest_absolute(&self.cwd) || self.workspace.as_ref().is_some_and(|p| !guest_absolute(p))
        {
            return Err(invalid("guest paths must be absolute"));
        }
        for path in std::iter::once(&self.cwd).chain(self.workspace.iter()) {
            CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest paths contain NUL"))?;
        }
        if let Some(path) = self
            .process
            .as_ref()
            .and_then(|options| options.allocator_reclaim.as_ref())
        {
            let path = path
                .to_str()
                .ok_or_else(|| invalid("invalid reclaim SO path"))?;
            if !guest_absolute(Path::new(path))
                || path.contains(['\0', ':', '$'])
                || path.chars().any(char::is_whitespace)
            {
                return Err(invalid(
                    "reclaim SO needs an absolute loader-safe guest path",
                ));
            }
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
            if !guest_absolute(&sidecar.log) {
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
        if (self.control || self.serial_control)
            && !self
                .process
                .as_ref()
                .is_some_and(|process| process.new_session)
        {
            return Err(invalid(
                "guest control requires a separate workload session",
            ));
        }
        if self.serial_control {
            let safe = self.process.as_ref().is_some_and(|p| {
                matches!(p.stdin, StdinMode::Closed | StdinMode::Pipe)
                    && p.capabilities.as_ref().is_some_and(|caps| {
                        !caps.iter().any(|c| [2, 16, 17, 19, 21, 27].contains(c))
                    })
            });
            if self.control || !safe {
                return Err(invalid(
                    "serial control requires explicit stdin ownership, restricted capabilities and no vsock adapter",
                ));
            }
        }
        if let Some(root) = &self.root_disk
            && (root.device.len() != 8
                || !root.device.starts_with("/dev/vd")
                || !root.device.as_bytes()[7].is_ascii_lowercase()
                || root.layers.len() > 128
                || root.layers.iter().any(|l| {
                    l.sha256.len() != 64
                        || !l
                            .sha256
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        || !matches!(
                            l.media_type.as_str(),
                            "application/vnd.oci.image.layer.v1.tar"
                                | "application/vnd.oci.image.layer.v1.tar+gzip"
                                | "application/vnd.docker.image.rootfs.diff.tar.gzip"
                        )
                }))
        {
            return Err(invalid("invalid root disk or OCI layer"));
        }
        for mount in &self.mounts {
            if mount.source_file && mount.initialize {
                return Err(invalid("file shares cannot initialize managed volumes"));
            }
            if !guest_mount_target(&mount.target) {
                return Err(invalid("invalid guest mount target"));
            }
            CString::new(mount.target.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest mount target contains NUL"))?;
            CString::new(mount.tag.as_str())
                .map_err(|_| invalid("guest mount tag contains NUL"))?;
        }
        if let Some(scratch) = &self.temporary_filesystem {
            if scratch.size_bytes == 0
                || !guest_mount_target(&scratch.path)
                || self.workspace.as_ref().is_some_and(|workspace| {
                    guest_path_starts_with(&scratch.path, workspace)
                        || guest_path_starts_with(workspace, &scratch.path)
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
            .is_some_and(|dir| !guest_absolute(dir))
        {
            return Err(invalid("guest payloads mount must be absolute"));
        }
        if self.shares.iter().any(|share| {
            share.tag.is_empty()
                || share.tag.contains('\0')
                || !guest_absolute(&share.path)
                || guest_components(&share.path).next().is_none()
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_preload_is_optional_and_validates_loader_paths() {
        let mut config: GuestConfig =
            serde_json::from_str(r#"{"argv":["/bin/true"],"env":{},"cwd":"/"}"#).unwrap();
        assert!(config.process.is_none());
        config.process = Some(ProcessConfig::default());
        assert!(
            serde_json::to_value(&config).unwrap()["process"]
                .get("allocator_reclaim")
                .is_none()
        );
        for path in [
            "relative.so",
            "/lib/with space.so",
            "/lib/a:b.so",
            "/lib/$LIB.so",
            "/lib/a\0b",
        ] {
            config.process.as_mut().unwrap().allocator_reclaim = Some(path.into());
            assert!(config.validate().is_err(), "{path:?}");
        }
        config.process.as_mut().unwrap().allocator_reclaim =
            Some("/opt/pvisor/libreclaim.so".into());
        config.validate().unwrap();
        let decoded: GuestConfig =
            serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(
            decoded.process.unwrap().allocator_reclaim.unwrap(),
            Path::new("/opt/pvisor/libreclaim.so")
        );
    }

    #[test]
    fn guest_paths_use_linux_syntax_on_every_host() {
        let mut config: GuestConfig = serde_json::from_str(
            r#"{"argv":["/bin/true"],"env":{},"cwd":"/work","workspace":"/work"}"#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
        for path in ["relative", r"C:\work", r"\\server\share", r"\work"] {
            config.cwd = path.into();
            assert!(config.validate().is_err(), "accepted host cwd {path:?}");
        }
        config.cwd = "/work".into();
        config.temporary_filesystem = Some(TemporaryFilesystem {
            path: "/worker/tmp".into(),
            size_bytes: 4096,
        });
        assert!(
            config.validate().is_ok(),
            "component prefixes must not overlap"
        );
        for path in ["/work/tmp", "//work/./tmp", "/work/../tmp", "/"] {
            config.temporary_filesystem.as_mut().unwrap().path = path.into();
            assert!(config.validate().is_err(), "accepted scratch {path:?}");
        }
        // A backslash is a normal Linux filename byte, not a path separator.
        config.temporary_filesystem.as_mut().unwrap().path = r"/work\tmp".into();
        assert!(config.validate().is_ok());
        config.payloads_dir = Some("/payloads".into());
        config.shares.push(GuestShare {
            tag: "data".into(),
            path: "/data".into(),
            read_only: true,
        });
        assert!(config.validate().is_ok());
        config.payloads_dir = Some(r"C:\payloads".into());
        assert!(config.validate().is_err());
        config.payloads_dir = None;
        config.shares[0].path = r"C:\data".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn shared_process_options_preserve_old_defaults_and_validate_before_launch() {
        let mut config: GuestConfig =
            serde_json::from_str(r#"{"argv":["/bin/true"],"env":{},"cwd":"/"}"#).unwrap();
        assert!(config.process.is_none());
        assert!(!ProcessConfig::default().attached);
        assert!(
            !serde_json::from_str::<ProcessConfig>("{}")
                .unwrap()
                .attached
        );
        assert!(!config.control);
        assert!(config.mounts.is_empty());
        assert!(config.validate().is_ok());
        config.control = true;
        assert!(config.validate().is_err());
        config.process = Some(ProcessConfig {
            new_session: true,
            ..Default::default()
        });
        assert!(config.validate().is_ok());
        config.process.as_mut().unwrap().tty = Some([0, 80]);
        assert!(config.validate().is_err());
        config.process.as_mut().unwrap().tty = Some([24, 80]);
        config.process.as_mut().unwrap().capabilities = Some(vec![64]);
        assert!(config.validate().is_err());
        config.process.as_mut().unwrap().capabilities = Some(vec![0, 1]);
        config.mounts.push(Mount {
            tag: "data".into(),
            target: "/data".into(),
            read_only: true,
            initialize: false,
            source_file: false,
        });
        assert!(config.validate().is_ok());
        config.mounts[0].source_file = true;
        config.mounts[0].initialize = true;
        assert!(config.validate().is_err());
        config.mounts[0].initialize = false;
        assert!(config.validate().is_ok());
        for path in ["/", "relative", "/data/../proc", "/bad\0path"] {
            config.mounts[0].target = path.into();
            assert!(config.validate().is_err(), "accepted {path:?}");
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
        assert!(config.validate().is_ok());
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
            assert!(config.validate().is_err(), "accepted {path:?}");
        }
        let scratch = config.temporary_filesystem.as_mut().unwrap();
        scratch.path = "/.pvisor-tmp-test".into();
        scratch.size_bytes = 0;
        assert!(config.validate().is_err());
    }
}

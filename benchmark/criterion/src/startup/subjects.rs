use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) const PAYLOAD: &str = "printf 'REFERENCE_READY\\nREFERENCE_RESULT {\"mode\":\"ready\",\"correctness\":\"passed\"}\\n'";
pub(super) const BOOT: &str = "console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/bench/init quiet pvbench.mode=ready pvbench.scratch=executor";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Backend {
    Native,
    PvisorHost,
    PvisorStaged,
    PvisorVm,
    Docker,
    FcSystem,
    Qemu,
    QemuMicrovm,
}

pub(super) const BACKENDS: [Backend; 8] = [
    Backend::Native,
    Backend::PvisorHost,
    Backend::PvisorStaged,
    Backend::PvisorVm,
    Backend::Docker,
    Backend::FcSystem,
    Backend::Qemu,
    Backend::QemuMicrovm,
];

impl Backend {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::PvisorHost => "pvisor-host",
            Self::PvisorStaged => "pvisor-staged",
            Self::PvisorVm => "pvisor-vm",
            Self::Docker => "docker",
            Self::FcSystem => "fc-system",
            Self::Qemu => "qemu",
            Self::QemuMicrovm => "qemu-microvm",
        }
    }
    pub(super) fn entry(self) -> String {
        format!("{}_timing.json", self.name().replace('-', "_"))
    }
    pub(super) fn pvisor(self) -> bool {
        matches!(self, Self::PvisorHost | Self::PvisorStaged | Self::PvisorVm)
    }
    pub(super) fn staged(self) -> bool {
        matches!(self, Self::PvisorStaged | Self::PvisorVm)
    }
    pub(super) fn guest(self) -> bool {
        matches!(self, Self::FcSystem | Self::Qemu | Self::QemuMicrovm)
    }
}

pub(super) struct Inputs {
    pub binary: PathBuf,
    pub assets: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub vmlinuz: Option<PathBuf>,
    pub initrd: Option<PathBuf>,
    pub image: Option<String>,
    pub cpus: String,
    pub batch: String,
    pub programs: BTreeMap<String, PathBuf>,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) fn argv(backend: Backend, input: &Inputs, trial: &Path) -> Result<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut add = |values: &[&str]| args.extend(values.iter().map(|value| value.to_string()));
    match backend {
        Backend::Native => add(&["/bin/sh", "-c", PAYLOAD]),
        Backend::PvisorHost | Backend::PvisorStaged | Backend::PvisorVm => {
            add(&[
                &text(&input.binary),
                "spawn",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--stdio",
                "inherit",
                "--timeout",
                "30s",
            ]);
            if backend == Backend::PvisorHost {
                add(&["--filesystem", "host"]);
            } else {
                add(&["--stage", &text(&trial.join("stage"))]);
            }
            if backend == Backend::PvisorVm {
                let assets = input
                    .assets
                    .as_ref()
                    .context("VM requires prepared assets")?;
                add(&[
                    "--vm",
                    "--rootfs",
                    &text(&assets.join("rootfs")),
                    "--cpu",
                    "2",
                    "--memory",
                    "128MiB",
                ]);
            }
            let shell = if backend == Backend::PvisorVm {
                "/bin/sh".to_string()
            } else {
                text(
                    input
                        .programs
                        .get("/bin/sh")
                        .context("host shell identity not frozen")?,
                )
            };
            add(&["--", &shell, "-c", PAYLOAD]);
        }
        Backend::Docker => {
            let image = input
                .image
                .as_deref()
                .context("Docker requires an immutable image")?;
            ensure!(
                image.starts_with("sha256:"),
                "Docker image must be immutable"
            );
            add(&[
                "docker",
                "run",
                "--rm",
                "--pull",
                "never",
                "--network",
                "none",
                "--name",
                &container_name(input, trial),
                "--label",
                &format!("pvisor.startup.batch={}", input.batch),
                "--mount",
                &format!(
                    "type=bind,source={},target=/work",
                    trial.join("workspace").display()
                ),
                "--workdir",
                "/work",
                "--entrypoint",
                "/bench/affinity",
                image,
                &input.cpus,
                "/bin/sh",
                "-c",
                PAYLOAD,
            ]);
        }
        Backend::FcSystem => add(&[
            "firecracker",
            "--no-api",
            "--config-file",
            &text(&trial.join("firecracker.json")),
        ]),
        Backend::Qemu | Backend::QemuMicrovm => {
            let micro = backend == Backend::QemuMicrovm;
            let kernel = input
                .vmlinuz
                .as_ref()
                .context("QEMU requires stock vmlinuz")?;
            let initrd = input
                .initrd
                .as_ref()
                .context("QEMU requires stock initrd")?;
            let machine = if micro {
                "microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=on"
            } else {
                "q35"
            };
            let boot = if micro {
                format!("{} pci=off", BOOT.replace("reboot=k", "reboot=t"))
            } else {
                BOOT.to_string()
            };
            add(&[
                "qemu-system-x86_64",
                "-machine",
                machine,
                "-accel",
                "kvm",
                "-cpu",
                "host",
                "-smp",
                "2",
                "-m",
                "128",
                "-nodefaults",
                "-display",
                "none",
                "-serial",
                "stdio",
                "-no-reboot",
                "-kernel",
                &text(kernel),
                "-append",
                &boot,
                "-drive",
                &format!(
                    "file={},format=raw,if=none,id=root",
                    trial.join("rootfs.ext4").display()
                ),
                "-device",
                if micro {
                    "virtio-blk-device,drive=root"
                } else {
                    "virtio-blk-pci,drive=root"
                },
                "-initrd",
                &text(initrd),
            ]);
        }
    }
    if !backend.pvisor() {
        args[0] = text(
            input
                .programs
                .get(&args[0])
                .context("subject executable identity not frozen")?,
        );
    }
    Ok(args)
}

pub(super) fn container_name(input: &Inputs, trial: &Path) -> String {
    format!(
        "{}-{}",
        input.batch,
        trial.file_name().unwrap().to_string_lossy()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inputs() -> Inputs {
        Inputs {
            binary: "/bin/pvisor".into(),
            assets: Some("/assets".into()),
            kernel: Some("/kernel".into()),
            vmlinuz: Some("/vmlinuz".into()),
            initrd: Some("/initrd".into()),
            image: Some(format!("sha256:{}", "a".repeat(64))),
            cpus: "0,1".into(),
            batch: "batch".into(),
            programs: BTreeMap::from([
                ("/bin/sh".into(), "/frozen/sh".into()),
                ("docker".into(), "/frozen/docker".into()),
                ("firecracker".into(), "/frozen/firecracker".into()),
                ("qemu-system-x86_64".into(), "/frozen/qemu".into()),
            ]),
        }
    }
    #[test]
    fn fixed_subjects_preserve_boundaries() {
        for backend in BACKENDS {
            let args = argv(backend, &inputs(), Path::new("/batch/trial")).unwrap();
            assert!(!args.iter().any(|arg| arg.contains("@")));
            assert!(!args.iter().any(|arg| arg == "python3"));
            if !backend.pvisor() {
                assert!(args[0].starts_with("/frozen/"));
            }
            if backend.pvisor() {
                assert!(args.iter().any(|arg| arg == "inherit"));
                assert!(args.iter().any(|arg| arg == PAYLOAD));
            }
            if backend == Backend::QemuMicrovm {
                assert!(args.iter().any(|arg| arg.contains("pci=off")));
            }
        }
    }
}

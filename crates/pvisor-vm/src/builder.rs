use crate::{
    backend::{Architecture, Backend, NativeBackend},
    firmware::KernelOwner,
    handle::Handle,
    polly::event_manager::EventManager,
    vmm::{
        resources::{TsiFlags, VmResources},
        vmm_config::{
            kernel_cmdline::{KernelCmdlineConfig, DEFAULT_KERNEL_CMDLINE},
            machine_config::VmConfig,
            vsock::VsockDeviceConfig,
        },
    },
};
#[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
use crate::{
    devices::virtio::fs::{
        passthrough::PermissionSemantics,
        virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent},
        OverlayConfig,
    },
    vmm::vmm_config::fs::FsDeviceConfig,
};
use std::{
    collections::HashMap,
    fs::File,
    io,
    marker::PhantomData,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Single-use, owned configuration. `run` consumes it and keeps firmware and
/// synthetic file payloads alive through the event loop. One runner owns one VM.
pub(crate) struct Builder<B: Backend = NativeBackend> {
    resources: VmResources,
    kernel: KernelOwner,
    implicit_init: bool,
    guest_cmdline: Option<KernelCmdlineConfig>,
    vsock_ports: HashMap<u32, (PathBuf, bool)>,
    backend: PhantomData<B>,
}
impl Builder<NativeBackend> {
    pub fn new(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        Self::for_backend(cpus, memory_mib)
    }
}
impl<B: Backend> Builder<B> {
    pub fn for_backend(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        let mut resources = VmResources::default();
        resources
            .set_vm_config(&VmConfig {
                vcpu_count: Some(cpus),
                mem_size_mib: Some(memory_mib as usize),
                ht_enabled: Some(false),
                cpu_template: None,
            })
            .map_err(io::Error::other)?;
        Ok(Self {
            resources,
            kernel: KernelOwner::default(),
            implicit_init: cfg!(feature = "init-blob"),
            guest_cmdline: None,
            vsock_ports: HashMap::new(),
            backend: PhantomData,
        })
    }
    pub fn ram_backing(&mut self, file: File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RAM backing must be an empty regular file",
            ));
        }
        self.resources.ram_backing = Some(Arc::new(file));
        Ok(())
    }
    pub fn embedded_kernel(
        &mut self,
        bytes: &[u8],
        guest_addr: u64,
        entry_addr: u64,
    ) -> io::Result<()> {
        // Validate before replacing the owner's mapping: a rejected update must
        // not leave the previous bundle pointing into a freed allocation.
        let mut owner = KernelOwner::default();
        let bundle = owner.embedded(bytes, guest_addr, entry_addr)?;
        self.resources
            .set_kernel_bundle(bundle)
            .map_err(io::Error::other)?;
        self.kernel = owner;
        Ok(())
    }
    /// Call before attaching rootfs; native-init restore must not inject init.krun.
    pub fn disable_implicit_init(&mut self) -> io::Result<()> {
        #[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
        if self.resources.fs.iter().any(|fs| fs.fs_id == "/dev/root") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configure init before rootfs",
            ));
        }
        self.implicit_init = false;
        Ok(())
    }
    pub fn guest_command(&mut self, command: crate::api::GuestCommand) -> io::Result<()> {
        if self.implicit_init {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "custom command requires a caller-supplied init",
            ));
        }
        let cmdline = command_line::<B::Arch>(&command)?;
        self.guest_cmdline = Some(cmdline);
        Ok(())
    }
    /// Explicit zero-feature vsock: no implicit host-network TSI fallback.
    pub fn vsock_port(
        &mut self,
        guest_port: u32,
        host_path: PathBuf,
        listen: bool,
    ) -> io::Result<()> {
        if self.vsock_ports.contains_key(&guest_port) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "duplicate vsock port",
            ));
        }
        self.vsock_ports.insert(guest_port, (host_path, listen));
        Ok(())
    }
    #[cfg(feature = "net")]
    pub fn network(
        &mut self,
        stream: std::os::unix::net::UnixStream,
        mac: [u8; 6],
        options: crate::api::NetworkOptions,
    ) -> io::Result<()> {
        use crate::{
            devices::virtio::net::device::VirtioNetBackend,
            vmm::vmm_config::net::NetworkInterfaceConfig,
        };
        // Configuration and workers share a valid owned descriptor; each worker
        // duplicates it, so boot failures and retries cannot leak or double-close.
        self.resources
            .net
            .insert(NetworkInterfaceConfig {
                iface_id: format!("eth{}", self.resources.net.list.len()),
                backend: VirtioNetBackend::UnixstreamFd(Arc::new(stream.into())),
                mac,
                features: 0,
            })
            .map_err(io::Error::other)?;
        self.resources.dhcp_client |= options.guest_dhcp;
        Ok(())
    }
    /// Build, invoke the ready callback, then enter the runner's event loop.
    /// Guest shutdown exits this process, as in the previous isolated runner.
    pub fn run(mut self, on_ready: impl FnOnce(Handle) -> io::Result<()>) -> io::Result<()> {
        if self.resources.kernel_bundle.is_none()
            && self.resources.external_kernel.is_none()
            && self.resources.firmware_config.is_none()
            && !cfg!(feature = "efi")
        {
            #[cfg(not(target_env = "musl"))]
            self.resources
                .set_kernel_bundle(self.kernel.load()?)
                .map_err(io::Error::other)?;
            #[cfg(target_env = "musl")]
            {
                let image = crate::firmware::embedded_kernel().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "configure an embedded kernel for this static build",
                    )
                })?;
                self.embedded_kernel(&image.bytes, image.guest_address, image.entry_address)?;
            }
        }
        self.resources
            .set_kernel_cmdline(
                self.guest_cmdline
                    .take()
                    .unwrap_or_else(|| KernelCmdlineConfig {
                        prolog: Some(format!("{DEFAULT_KERNEL_CMDLINE} init=/init.krun")),
                        krun_env: Some("     ".into()),
                        epilog: Some(" -- ".into()),
                    }),
            )
            .map_err(io::Error::other)?;
        self.resources
            .set_vsock_device(VsockDeviceConfig {
                vsock_id: "vsock0".into(),
                guest_cid: 3,
                host_port_map: None,
                unix_ipc_port_map: if self.vsock_ports.is_empty() {
                    None
                } else {
                    Some(self.vsock_ports)
                },
                tsi_flags: TsiFlags::empty(),
            })
            .map_err(io::Error::other)?;
        let mut events =
            EventManager::new().map_err(|e| io::Error::other(format!("event manager: {e:?}")))?;
        let (sender, receiver) = crossbeam_channel::unbounded();
        let vm = B::build(&self.resources, &mut events, None, sender).map_err(|e| {
            io::Error::other(format!("build {} {} VM: {e:?}", B::NAME, B::Arch::NAME))
        })?;
        on_ready(Handle {
            vmm: Arc::downgrade(&vm),
            transition: Arc::new(Mutex::new(())),
        })?;
        B::start_worker(&self.resources, vm.clone(), receiver)?;
        loop {
            events
                .run()
                .map_err(|e| io::Error::other(format!("VM event loop: {e:?}")))?;
        }
    }
}
#[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
impl<B: Backend> Builder<B> {
    fn attach_fs(&mut self, mut config: FsDeviceConfig) -> io::Result<()> {
        if config.fs_id.is_empty() || self.resources.fs.iter().any(|fs| fs.fs_id == config.fs_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty or duplicate filesystem tag",
            ));
        }
        #[cfg(feature = "init-blob")]
        if config.fs_id == "/dev/root" && self.implicit_init {
            config.virtual_entries.push(VirtualDirEntry {
                name: std::ffi::CString::new("init.krun").expect("literal"),
                entry: VirtualEntry {
                    mode: 0o755,
                    one_shot: true,
                    content: VirtualEntryContent::File {
                        data: Arc::from(include_bytes!(env!("PVISOR_GUEST_BINARY")).as_slice()),
                    },
                },
            });
        }
        self.resources.add_fs_device(config);
        Ok(())
    }
    pub fn filesystem(
        &mut self,
        tag: &str,
        path: &std::path::Path,
        shm_size: usize,
    ) -> io::Result<()> {
        self.attach_fs(FsDeviceConfig {
            fs_id: tag.into(),
            shared_dir: Some(
                path.to_str()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "filesystem path is not UTF-8")
                    })?
                    .into(),
            ),
            semantics: PermissionSemantics::LinuxComplete,
            shm_size: (shm_size != 0).then_some(shm_size),
            read_only: false,
            overlay: None,
            virtual_entries: Vec::new(),
        })
    }
    pub fn overlay(
        &mut self,
        tag: &str,
        overlay: OverlayConfig,
        shm_size: usize,
    ) -> io::Result<()> {
        if overlay.lower_dirs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "overlay requires a lower directory",
            ));
        }
        self.attach_fs(FsDeviceConfig {
            fs_id: tag.into(),
            shared_dir: None,
            semantics: overlay.semantics,
            shm_size: (shm_size != 0).then_some(shm_size),
            read_only: false,
            overlay: Some(overlay),
            virtual_entries: Vec::new(),
        })
    }
    pub fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: impl Into<Arc<[u8]>>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()> {
        let fs = self
            .resources
            .fs
            .iter_mut()
            .find(|fs| fs.fs_id == tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown filesystem tag"))?;
        let components: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
        if components
            .iter()
            .any(|c| c.is_empty() || *c == "." || *c == ".." || c.contains('\0'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid virtual file path",
            ));
        }
        let (leaf, parents) = components
            .split_last()
            .expect("split always has a component");
        let mut entries = &mut fs.virtual_entries;
        for component in parents {
            let dir = entries
                .iter_mut()
                .find(|e| e.name.as_bytes() == component.as_bytes())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "virtual parent does not exist")
                })?;
            match &mut dir.entry.content {
                VirtualEntryContent::Dir { children } => entries = children,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "virtual parent is a file",
                    ))
                }
            }
        }
        if entries.iter().any(|e| e.name.as_bytes() == leaf.as_bytes()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "duplicate virtual entry",
            ));
        }
        entries.push(VirtualDirEntry {
            name: std::ffi::CString::new(*leaf).map_err(io::Error::other)?,
            entry: VirtualEntry {
                mode,
                one_shot,
                content: VirtualEntryContent::File { data: data.into() },
            },
        });
        Ok(())
    }
}
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
impl<B: crate::backend::SnapshotBackend> Builder<B> {
    pub fn snapshot_profile(&mut self) -> io::Result<()> {
        if self.resources.nested_enabled {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshot does not support nested virtualization",
            ));
        }
        self.resources.snapshot_profile = true;
        Ok(())
    }
    pub fn machine_restore(
        &mut self,
        restore: crate::vmm::snapshot::MachineRestore,
    ) -> io::Result<()> {
        self.snapshot_profile()?;
        self.resources.machine_restore = Some(Arc::new(restore));
        Ok(())
    }
}

/// Install logging if the embedding process has not already installed a logger.
pub fn init_logging(filter: &str) {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
        .format_timestamp_micros()
        .try_init();
}

/// Custom init has a narrow, validated command contract instead of exposing a
/// raw kernel-command-line setter or inheriting arbitrary host environment.
fn command_line<A: Architecture>(
    command: &crate::api::GuestCommand,
) -> io::Result<KernelCmdlineConfig> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    fn printable(value: &str) -> bool {
        value
            .bytes()
            .all(|b| (b' '..=b'~').contains(&b) && b != b'"')
    }
    for path in [&command.executable, &command.working_directory] {
        if !path.starts_with('/') || !printable(path) || path.contains(' ') {
            return Err(invalid(
                "custom init paths must be absolute printable paths without spaces or quotes",
            ));
        }
    }
    if command.arguments.iter().any(|value| !printable(value)) {
        return Err(invalid("unsupported custom init argument quoting"));
    }
    let mut env = Vec::with_capacity(command.environment.len());
    for (key, value) in &command.environment {
        if key.is_empty()
            || key.starts_with("KRUN_")
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !printable(value)
        {
            return Err(invalid("invalid custom init environment entry"));
        }
        env.push(format!("\"{key}={value}\""));
    }
    let config = KernelCmdlineConfig {
        prolog: Some(format!("{DEFAULT_KERNEL_CMDLINE} init=/init.krun")),
        krun_env: Some(format!(
            " KRUN_INIT={} KRUN_WORKDIR={}   {}",
            command.executable,
            command.working_directory,
            env.join(" ")
        )),
        epilog: Some(format!(
            " -- {}",
            command
                .arguments
                .iter()
                .map(|arg| format!("\"{arg}\""))
                .collect::<Vec<_>>()
                .join(" ")
        )),
    };
    let mut checked = crate::kernel::cmdline::Cmdline::new(A::CMDLINE_MAX_SIZE);
    for part in [&config.prolog, &config.krun_env, &config.epilog]
        .into_iter()
        .flatten()
    {
        checked
            .insert_str(part)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    }
    Ok(config)
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::api::GuestCommand;

    fn command() -> GuestCommand {
        GuestCommand {
            executable: "/payload".into(),
            arguments: vec![],
            environment: Default::default(),
            working_directory: "/".into(),
        }
    }

    #[test]
    fn custom_init_preserves_the_explicit_empty_legacy_command_contract() {
        let result = command_line::<<NativeBackend as Backend>::Arch>(&command()).unwrap();
        assert_eq!(
            result.krun_env.as_deref(),
            Some(" KRUN_INIT=/payload KRUN_WORKDIR=/   ")
        );
        assert_eq!(result.epilog.as_deref(), Some(" -- "));
    }

    #[test]
    fn invalid_custom_commands_preserve_the_previous_configuration() {
        let mut builder = Builder::new(1, 128).unwrap();
        assert!(builder.guest_command(command()).is_err());
        builder.disable_implicit_init().unwrap();
        builder.guest_command(command()).unwrap();
        let saved = builder.guest_cmdline.clone();
        let mut bad = command();
        bad.arguments.push("injected\" argument".into());
        assert!(builder.guest_command(bad).is_err());
        assert_eq!(builder.guest_cmdline, saved);
        let mut bad = command();
        bad.environment.insert("KRUN_INIT".into(), "/other".into());
        assert!(builder.guest_command(bad).is_err());
        assert_eq!(builder.guest_cmdline, saved);
        let mut bad = command();
        bad.arguments
            .push("x".repeat(<NativeBackend as Backend>::Arch::CMDLINE_MAX_SIZE));
        assert!(builder.guest_command(bad).is_err());
        assert_eq!(builder.guest_cmdline, saved);
    }

    #[cfg(feature = "net")]
    #[test]
    fn dhcp_is_explicit_and_the_runtime_owns_the_network_descriptor() {
        use std::io::Read;
        let mut builder = Builder::new(1, 128).unwrap();
        let (stream, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        builder
            .network(
                stream,
                [2, 0, 0, 0, 0, 1],
                crate::api::NetworkOptions::default(),
            )
            .unwrap();
        assert!(!builder.resources.dhcp_client);
        drop(builder);
        // Dropping unbooted configuration closes its owned transport, without a worker leak.
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        let mut builder = Builder::new(1, 128).unwrap();
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        builder
            .network(
                stream,
                [2, 0, 0, 0, 0, 1],
                crate::api::NetworkOptions { guest_dhcp: true },
            )
            .unwrap();
        assert!(builder.resources.dhcp_client);
    }
}

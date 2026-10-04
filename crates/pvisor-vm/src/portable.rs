//! Implement the uniform API; all platform decisions stay behind this module.
use crate::api::*;
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

impl RuntimeSupport for VmPlatform {
    fn init_logging(filter: &str) {
        crate::builder::init_logging(filter);
    }
    fn capabilities() -> Capabilities {
        capabilities()
    }
    fn embedded_kernel() -> Option<KernelImage> {
        crate::firmware::embedded_kernel()
    }
    fn firmware_name() -> &'static str {
        crate::firmware_store::firmware_name()
    }
    fn firmware_version() -> &'static str {
        crate::firmware_store::VERSION
    }
    fn bundled_firmware_directory() -> Option<PathBuf> {
        crate::firmware_store::bundled_directory()
    }
    fn prepare_firmware(cache_root: Option<&Path>) -> io::Result<PathBuf> {
        crate::firmware_store::prepare(cache_root)
    }

    fn cold_ram_activity() -> ColdRamActivity {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            crate::cold_ram::activity()
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            ColdRamActivity::default()
        }
    }
}
fn capabilities() -> Capabilities {
    Capabilities {
        architecture: {
            #[cfg(target_arch = "aarch64")]
            {
                Architecture::Aarch64
            }
            #[cfg(target_arch = "x86_64")]
            {
                Architecture::X86_64
            }
            #[cfg(target_arch = "riscv64")]
            {
                Architecture::Riscv64
            }
        },
        hypervisor: {
            #[cfg(target_os = "macos")]
            {
                Hypervisor::Hvf
            }
            #[cfg(target_os = "linux")]
            {
                Hypervisor::Kvm
            }
        },
        machine_snapshot: cfg!(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )) && !cfg!(any(feature = "tee", feature = "aws-nitro")),
        cold_ram_faults: cfg!(all(target_os = "macos", target_arch = "aarch64")),
    }
}
fn unsupported(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{operation} is unsupported by this VM backend"),
    )
}

impl VmConfiguration for VmBuilder {
    fn from_config(config: VmConfig) -> io::Result<Self> {
        Self::new(config.cpus, config.memory_mib)
    }
    fn new(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        Ok(Self {
            inner: crate::builder::Builder::new(cpus, memory_mib)?,
        })
    }
    fn ram_backing(&mut self, file: File) -> io::Result<()> {
        self.inner.ram_backing(file)
    }
    fn embedded_kernel(
        &mut self,
        bytes: &[u8],
        guest_addr: u64,
        entry_addr: u64,
    ) -> io::Result<()> {
        self.inner.embedded_kernel(bytes, guest_addr, entry_addr)
    }
    fn disable_implicit_init(&mut self) -> io::Result<()> {
        self.inner.disable_implicit_init()
    }
    fn guest_command(&mut self, command: GuestCommand) -> io::Result<()> {
        self.inner.guest_command(command)
    }
    fn vsock_port(&mut self, guest_port: u32, host_path: PathBuf, listen: bool) -> io::Result<()> {
        self.inner.vsock_port(guest_port, host_path, listen)
    }
    fn network(&mut self, stream: std::os::unix::net::UnixStream, mac: [u8; 6]) -> io::Result<()> {
        self.network_with_options(stream, mac, NetworkOptions::default())
    }
    fn network_with_options(
        &mut self,
        stream: std::os::unix::net::UnixStream,
        mac: [u8; 6],
        options: NetworkOptions,
    ) -> io::Result<()> {
        #[cfg(feature = "net")]
        {
            self.inner.network(stream, mac, options)
        }
        #[cfg(not(feature = "net"))]
        {
            let _ = (stream, mac, options);
            Err(unsupported("virtio-net"))
        }
    }
    fn filesystem(&mut self, tag: &str, path: &Path, shm_size: usize) -> io::Result<()> {
        #[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
        {
            self.inner.filesystem(tag, path, shm_size)
        }
        #[cfg(any(feature = "tee", feature = "aws-nitro"))]
        {
            let _ = (tag, path, shm_size);
            Err(unsupported("virtio-fs"))
        }
    }
    fn overlay(&mut self, tag: &str, overlay: OverlayConfig, shm_size: usize) -> io::Result<()> {
        #[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
        {
            let path = |path: &Path| -> io::Result<String> {
                path.to_str().map(str::to_owned).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("VM filesystem path is not UTF-8: {}", path.display()),
                    )
                })
            };
            use crate::devices::virtio::fs::passthrough::PermissionSemantics as Internal;
            let semantics = match overlay.semantics {
                PermissionSemantics::LinuxComplete => Internal::LinuxComplete,
                PermissionSemantics::LinuxSimplified => Internal::LinuxSimplified,
            };
            self.inner.overlay(
                tag,
                crate::devices::virtio::fs::OverlayConfig {
                    lower_dirs: overlay
                        .lower_dirs
                        .iter()
                        .map(|p| path(p))
                        .collect::<io::Result<_>>()?,
                    upper_dir: path(&overlay.upper_dir)?,
                    work_dir: overlay.work_dir.as_deref().map(path).transpose()?,
                    preimage_dir: overlay.preimage_dir.as_deref().map(path).transpose()?,
                    apply_target: overlay.apply_target.as_deref().map(path).transpose()?,
                    baseline_lower: overlay.baseline_lower.as_deref().map(path).transpose()?,
                    excluded_paths: overlay
                        .excluded_paths
                        .iter()
                        .map(|p| path(p))
                        .collect::<io::Result<_>>()?,
                    access_policy: overlay.access_policy,
                    semantics,
                },
                shm_size,
            )
        }
        #[cfg(any(feature = "tee", feature = "aws-nitro"))]
        {
            let _ = (tag, overlay, shm_size);
            Err(unsupported("overlay filesystem"))
        }
    }
    fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: impl Into<Arc<[u8]>>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()> {
        #[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
        {
            self.inner.virtual_file(tag, path, data, mode, one_shot)
        }
        #[cfg(any(feature = "tee", feature = "aws-nitro"))]
        {
            let _ = (tag, path, data, mode, one_shot);
            Err(unsupported("virtual file"))
        }
    }
    fn snapshot_profile(&mut self) -> io::Result<()> {
        if !capabilities().machine_snapshot {
            return Err(unsupported("machine snapshot"));
        }
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.inner.snapshot_profile()
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            Err(unsupported("machine snapshot"))
        }
    }
    fn machine_restore(&mut self, restore: MachineRestore) -> io::Result<()> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.inner
                .machine_restore(crate::vmm::snapshot::MachineRestore {
                    state: serde_json::from_value(restore.state.state).map_err(io::Error::other)?,
                    ram_file: restore.ram_file,
                })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = restore;
            Err(unsupported("machine restore"))
        }
    }
}

impl VmRuntime for VmBuilder {
    fn run(self, on_ready: impl FnOnce(VmmHandle) -> io::Result<()>) -> io::Result<()> {
        self.inner.run(|inner| on_ready(VmmHandle { inner }))
    }
}

impl VmControl for VmmHandle {
    fn is_paused(&self) -> Result<bool, String> {
        self.inner.is_paused()
    }
    fn pause(&self) -> Result<(), String> {
        self.inner.pause()
    }
    fn resume(&self) -> Result<(), String> {
        self.inner.resume()
    }
    fn offload_ram(&self) -> Result<RamReclaim, String> {
        self.inner.offload_ram().map(|s| RamReclaim {
            backed_bytes: s.backed_bytes,
            resident_before_bytes: s.resident_before_bytes,
            resident_after_bytes: s.resident_after_bytes,
        })
    }
}
impl SnapshotControl for VmmHandle {
    fn with_snapshot_quiesced<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.inner
                .with_snapshot_quiesced(timeout, |inner| action(&mut FrozenMachine { inner }))
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (timeout, action);
            Err(unsupported("machine freeze").to_string())
        }
    }
}
impl ColdRamControl for VmmHandle {
    fn start_cold_pager<S: ColdRamStore + 'static>(
        &self,
        store: S,
        options: ColdRamOptions,
    ) -> io::Result<()> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            crate::cold_ram::start(self.clone(), store, options)
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = (store, options);
            Err(unsupported("experimental cold RAM pager"))
        }
    }
    fn with_ram_quiesced<T>(
        &self,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        #[cfg(target_os = "macos")]
        {
            self.inner
                .with_ram_quiesced(|inner| action(&mut FrozenMachine { inner }))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = action;
            Err(unsupported("cold RAM quiescence").to_string())
        }
    }
    fn experimental_ram_residency(&self) -> Result<Option<u64>, String> {
        #[cfg(target_os = "macos")]
        {
            self.inner.experimental_ram_residency()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(unsupported("cold RAM residency").to_string())
        }
    }
    fn install_ram_fault_handler(&self, handler: Arc<MemoryFaultHandler>) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.inner.install_ram_fault_handler(handler)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = handler;
            Err(unsupported("RAM fault handler").to_string())
        }
    }
}
impl SnapshotCapture for FrozenMachine<'_> {
    fn capture_machine_state(&self, file: &File) -> Result<MachineSnapshot, String> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            Ok(MachineSnapshot {
                state: serde_json::to_value(self.inner.capture_machine_state(file)?)
                    .map_err(|e| e.to_string())?,
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = file;
            Err(unsupported("machine capture").to_string())
        }
    }
}
impl FrozenMemory for FrozenMachine<'_> {
    fn experimental_ram_blocks(&self, bytes: usize) -> Result<Vec<RamBlock>, String> {
        #[cfg(target_os = "macos")]
        {
            self.inner
                .experimental_ram_blocks(bytes)
                .map(|v| v.into_iter().map(|inner| RamBlock { inner }).collect())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = bytes;
            Err(unsupported("cold RAM blocks").to_string())
        }
    }
    fn experimental_ram_page_inventory(&self) -> Result<(u64, Vec<[u64; 4]>), String> {
        #[cfg(target_os = "macos")]
        {
            self.inner.experimental_ram_page_inventory()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(unsupported("RAM page inventory").to_string())
        }
    }
    fn install_memory_fault_handler(
        &mut self,
        handler: Arc<MemoryFaultHandler>,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.inner
                .install_memory_fault_handler(handler)
                .map_err(|e| e.to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = handler;
            Err(unsupported("RAM fault handler").to_string())
        }
    }
    fn set_device_prepare(&self, handler: Option<Arc<MemoryPrepare>>) -> Result<(), String> {
        self.inner
            .device_memory_gate()
            .set_prepare(handler)
            .map_err(str::to_owned)
    }
}
impl SnapshotState for MachineSnapshot {
    fn cpu_count(&self) -> io::Result<usize> {
        self.state
            .get("cpus")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing CPU inventory"))
    }
    fn ram_mappings(&self) -> io::Result<Vec<RamMappingSnapshot>> {
        serde_json::from_value(
            self.state.get("ram").cloned().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing RAM inventory")
            })?,
        )
        .map_err(io::Error::other)
    }
    fn rebind_filesystem_copy(
        &mut self,
        tag: &str,
        source: &Path,
        destination: &Path,
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_copy(tag, source, destination)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, source, destination);
            Err(unsupported("filesystem snapshot rebinding"))
        }
    }
    fn rebind_filesystem_layers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_layers(tag, copies)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, copies);
            Err(unsupported("filesystem snapshot rebinding"))
        }
    }
}
impl MachineSnapshot {
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn rebind(
        &mut self,
        tag: &str,
        action: impl Fn(&mut crate::devices::virtio::MmioSnapshot, &[u8; 36]) -> io::Result<bool>,
    ) -> io::Result<usize> {
        if tag.is_empty() || tag.len() > 36 || tag.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid filesystem tag",
            ));
        }
        let mut bytes = [0; 36];
        bytes[..tag.len()].copy_from_slice(tag.as_bytes());
        let mut internal: crate::vmm::snapshot::MachineSnapshot =
            serde_json::from_value(self.state.clone()).map_err(io::Error::other)?;
        let mut count = 0;
        for mapping in &mut internal.devices {
            if let crate::devices::snapshot::BusDeviceSnapshot::Virtio(device) = &mut mapping.device
            {
                count += usize::from(action(device, &bytes)?);
            }
        }
        self.state = serde_json::to_value(internal).map_err(io::Error::other)?;
        Ok(count)
    }
}
impl RestoreState for MachineRestore {
    fn validate(&self, cpu_count: usize) -> Result<(), String> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.internal()?.validate(cpu_count)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = cpu_count;
            Err(unsupported("machine restore").to_string())
        }
    }
    fn map_ram(
        &self,
        ranges: &[(vm_memory::GuestAddress, usize)],
    ) -> Result<vm_memory::GuestMemoryMmap, String> {
        crate::memory::map_snapshot_ram(
            &self.state.ram_mappings().map_err(|e| e.to_string())?,
            self.ram_file.clone(),
            ranges,
        )
    }
}
impl MachineRestore {
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn internal(&self) -> Result<crate::vmm::snapshot::MachineRestore, String> {
        Ok(crate::vmm::snapshot::MachineRestore {
            state: serde_json::from_value(self.state.state.clone()).map_err(|e| e.to_string())?,
            ram_file: self.ram_file.clone(),
        })
    }
}

impl RamFileMapping for RamFileMount {
    fn mount(
        store: Arc<std::sync::Mutex<dyn RamFileStore>>,
        directory: &Path,
    ) -> io::Result<(Self, File)> {
        crate::ram_file::Mount::new(store, directory)
            .map(|(inner, file)| (Self { _inner: inner }, file))
    }
}

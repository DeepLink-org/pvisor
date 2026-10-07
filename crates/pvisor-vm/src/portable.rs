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
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
            .format_timestamp_micros()
            .try_init();
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

    fn bundled_firmware_directory() -> Option<PathBuf> {
        crate::firmware_store::bundled_directory()
    }
    fn resolve_firmware_path(directory: Option<&Path>) -> io::Result<PathBuf> {
        crate::firmware_store::resolve(directory)
    }

    fn cold_ram_activity() -> ColdRamActivity {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            crate::cold_ram::activity()
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            crate::cold_ram_linux::activity()
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
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
        cold_ram_faults: cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || (cfg!(all(target_os = "linux", target_arch = "x86_64"))
                && !cfg!(any(
                    feature = "tee",
                    feature = "aws-nitro",
                    feature = "gpu",
                    feature = "snd",
                    feature = "input"
                ))),
    }
}
#[cfg(all(test, target_os = "linux"))]
mod cold_ram_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex, Weak,
    };

    struct UnusedStore {
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for UnusedStore {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ColdRamStore for UnusedStore {
        type Object = ();

        fn put(&mut self, _: &[u8]) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported pager must not publish RAM")
        }

        fn restore(&mut self, _: &(), _: &mut [u8]) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported pager must not restore RAM")
        }

        fn release(&mut self, _: ()) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported pager must not release objects")
        }

        fn stats(&mut self) -> io::Result<ColdRamPoolStats> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported pager must not query storage")
        }
        fn shared_mapping(&self, _: &()) -> Option<SharedRamMapping> {
            None
        }
    }

    fn inactive_handle() -> VmmHandle {
        VmmHandle {
            vmm: Weak::new(),
            transition: Arc::new(Mutex::new(())),
            cold_pager_started: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn linux_cold_pager_rejects_without_storage_or_worker_activity() {
        let handle = inactive_handle();
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        for metrics in [false, true] {
            let error = handle
                .start_cold_pager(
                    UnusedStore {
                        calls: calls.clone(),
                        drops: drops.clone(),
                    },
                    ColdRamOptions {
                        metrics,
                        ..Default::default()
                    },
                )
                .unwrap_err();
            if cfg!(all(
                target_arch = "x86_64",
                not(any(
                    feature = "tee",
                    feature = "aws-nitro",
                    feature = "gpu",
                    feature = "snd",
                    feature = "input"
                ))
            )) {
                assert_eq!(error.kind(), io::ErrorKind::Other);
                assert_eq!(error.to_string(), "VMM has stopped");
            } else {
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            }
            assert!(!handle.cold_pager_started.load(Ordering::SeqCst));
            assert!(handle.transition.try_lock().is_ok());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(
            VmPlatform::capabilities().cold_ram_faults,
            cfg!(all(
                target_arch = "x86_64",
                not(any(
                    feature = "tee",
                    feature = "aws-nitro",
                    feature = "gpu",
                    feature = "snd",
                    feature = "input"
                ))
            ))
        );
        let activity = VmPlatform::cold_ram_activity();
        assert_eq!(activity.pending_file_bytes, 0);
        assert_eq!(activity.pending_snapshot_bytes, 0);
    }

    #[test]
    fn linux_cold_controls_reject_without_running_callbacks() {
        let handle = inactive_handle();
        let error = handle
            .with_ram_quiesced::<()>(|_| panic!("unsupported quiescence must not run callback"))
            .unwrap_err();
        assert_eq!(
            error,
            "cold RAM quiescence is unsupported by this VM backend"
        );
        assert_eq!(
            handle.experimental_ram_residency().unwrap_err(),
            "cold RAM residency is unsupported by this VM backend"
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let handler = Arc::new(move |_| {
            callback_calls.fetch_add(1, Ordering::SeqCst);
            panic!("unsupported fault handler must not be called")
        });
        assert_eq!(
            handle.install_ram_fault_handler(handler).unwrap_err(),
            "RAM fault handler is unsupported by this VM backend"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!handle.cold_pager_started.load(Ordering::SeqCst));
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
    fn from_restore(config: VmConfig, restore: MachineRestore) -> io::Result<Self> {
        let mut builder = Self::from_config(config)?;
        builder.machine_restore(restore)?;
        Ok(builder)
    }
    fn new(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        Ok(Self {
            inner: crate::builder::Builder::new(cpus, memory_mib)?,
        })
    }
    fn set_firmware_path(&mut self, path: PathBuf) -> io::Result<()> {
        self.inner.set_firmware_path(path)
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
                    baseline_content_index: overlay
                        .baseline_content_index
                        .map(|index| {
                            path(&index.root)?;
                            path(&index.file)?;
                            Ok::<_, io::Error>(index)
                        })
                        .transpose()?,
                    excluded_paths: overlay
                        .excluded_paths
                        .iter()
                        .map(|p| path(p))
                        .collect::<io::Result<_>>()?,
                    access_policy: overlay.access_policy,
                    semantics: overlay.semantics,
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
        self.inner.run(on_ready)
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
            self.snapshot_quiesced(timeout, |inner| action(&mut FrozenMachine { inner }))
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
    fn with_snapshot_frozen<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.snapshot_frozen(timeout, |inner| action(&mut FrozenMachine { inner }))
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
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            crate::cold_ram_linux::start(self.clone(), store, options)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
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
            self.ram_quiesced(|inner| action(&mut FrozenMachine { inner }))
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
            self.ram_residency()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(unsupported("cold RAM residency").to_string())
        }
    }
    fn install_ram_fault_handler(&self, handler: Arc<MemoryFaultHandler>) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.register_ram_fault_handler(handler)
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
    fn capture_machine_state_with_ram_delta(
        &self,
        file: &File,
        baseline: Option<&RamDeltaSpec>,
    ) -> Result<(MachineSnapshot, Option<RamDeltaCapture>), String> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            let (state, delta) = self
                .inner
                .capture_machine_state_with_ram_delta(file, baseline)?;
            Ok((
                MachineSnapshot {
                    state: serde_json::to_value(state).map_err(|e| e.to_string())?,
                },
                delta,
            ))
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (file, baseline);
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
    fn has_kernel_layout(&self) -> io::Result<bool> {
        match self.state.get("kernel_layout") {
            None | Some(serde_json::Value::Null) => Ok(false),
            Some(serde_json::Value::Object(_)) => Ok(true),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid snapshot kernel geometry",
            )),
        }
    }
    fn verify_frozen_filesystem_backing(&self, tag: &str) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            if tag.is_empty() || tag.len() > 36 || tag.contains('\0') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid filesystem tag",
                ));
            }
            let mut bytes = [0; 36];
            bytes[..tag.len()].copy_from_slice(tag.as_bytes());
            let internal: crate::vmm::snapshot::MachineSnapshot =
                serde::Deserialize::deserialize(&self.state).map_err(io::Error::other)?;
            let mut count = 0;
            for mapping in &internal.devices {
                if let crate::devices::snapshot::BusDeviceSnapshot::Virtio(device) = &mapping.device
                {
                    count += usize::from(device.verify_frozen_filesystem_backing(&bytes)?);
                }
            }
            Ok(count)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = tag;
            Err(unsupported("filesystem snapshot rebinding"))
        }
    }
    fn rebind_filesystem_lower_copies(
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
                device.rebind_filesystem_lower_copies(tag, copies)
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
    fn rebind_filesystem_shared_lowers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        shared_lowers: &[PathBuf],
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_shared_lowers(tag, copies, shared_lowers)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, copies, shared_lowers);
            Err(unsupported("filesystem snapshot rebinding"))
        }
    }
    fn rebind_filesystem_policy(
        &mut self,
        tag: &str,
        policy: &pvisor_overlay_core::FileAccessPolicy,
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_policy(tag, policy)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, policy);
            Err(unsupported("filesystem snapshot rebinding"))
        }
    }
    fn cpu_count(&self) -> io::Result<usize> {
        self.state
            .get("cpus")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing CPU inventory"))
    }
    fn rebind_filesystem_exclusions(
        &mut self,
        tag: &str,
        excluded_paths: &[PathBuf],
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_exclusions(tag, excluded_paths)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, excluded_paths);
            Err(unsupported("filesystem snapshot exclusions rebinding"))
        }
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
    fn rebind_filesystem_stage(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        immutable_lowers: &[PathBuf],
    ) -> io::Result<usize> {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            self.rebind(tag, |device, tag| {
                device.rebind_filesystem_stage(tag, copies, immutable_lowers)
            })
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        )))]
        {
            let _ = (tag, copies, immutable_lowers);
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

impl RamDeltaState for RamDeltaSpec {
    fn validate(&self) -> Result<(), String> {
        if self.length == 0
            || self.length > 64 * 1024 * 1024 * 1024
            || !self.block_bytes.is_power_of_two()
            || !(4096..=1024 * 1024).contains(&self.block_bytes)
            || self.length.div_ceil(u64::from(self.block_bytes)) > 1 << 20
            || self.base_sha256.len() != 64
            || !self
                .base_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid incremental RAM baseline".into());
        }
        Ok(())
    }
}
impl RamDeltaState for RamDeltaCapture {
    fn validate(&self) -> Result<(), String> {
        RamDeltaSpec {
            device: 0,
            inode: 0,
            length: self.length,
            block_bytes: self.block_bytes,
            base_sha256: self.base_sha256.clone(),
        }
        .validate()?;
        if self.version != 1
            || self.changed_blocks.len() as u64 > self.length.div_ceil(u64::from(self.block_bytes))
            || self
                .changed_blocks
                .iter()
                .any(|index| *index >= self.length.div_ceil(u64::from(self.block_bytes)))
            || self
                .changed_blocks
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err("invalid incremental RAM capture inventory".into());
        }
        Ok(())
    }
}

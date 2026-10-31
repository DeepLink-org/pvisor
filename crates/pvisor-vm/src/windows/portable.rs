//! WHP implementations of the uniform public contracts.
#![allow(unused_variables)]
use crate::api::*;
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
fn unsupported(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{operation} is unsupported on Windows/WHP"),
    )
}

impl VmConfiguration for VmBuilder {
    fn new(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        Ok(Self {
            inner: crate::builder::Builder::new(cpus, memory_mib)?,
        })
    }
    fn from_config(config: VmConfig) -> io::Result<Self> {
        Self::new(config.cpus, config.memory_mib)
    }
    fn from_restore(config: VmConfig, restore: MachineRestore) -> io::Result<Self> {
        Err(unsupported("from_restore"))
    }
    fn set_firmware_path(&mut self, path: PathBuf) -> io::Result<()> {
        self.inner.set_firmware_path(path)
    }
    fn ram_backing(&mut self, file: File) -> io::Result<()> {
        Err(unsupported("ram_backing"))
    }
    fn shared_kernel_base(&mut self, directory: PathBuf) -> io::Result<()> {
        Err(unsupported("shared_kernel_base"))
    }
    fn embedded_kernel(
        &mut self,
        bytes: &[u8],
        guest_addr: u64,
        entry_addr: u64,
    ) -> io::Result<()> {
        self.inner.embedded_kernel(bytes, guest_addr, entry_addr)
    }
    fn guest_idle_handoff(&mut self) -> io::Result<()> {
        Err(unsupported("guest_idle_handoff"))
    }
    fn guest_idle_handoff_with_policy(
        &mut self,
        disable_proactive_compaction: bool,
        offload_minimum: Duration,
    ) -> io::Result<()> {
        Err(unsupported("guest_idle_handoff_with_policy"))
    }
    fn disable_implicit_init(&mut self) -> io::Result<()> {
        self.inner.disable_implicit_init()
    }
    fn guest_command(&mut self, command: GuestCommand) -> io::Result<()> {
        Err(unsupported("guest_command"))
    }
    fn filesystem(&mut self, tag: &str, path: &Path, shm_size: usize) -> io::Result<()> {
        self.inner.filesystem(tag, path, shm_size, false)
    }
    fn read_only_filesystem(&mut self, tag: &str, path: &Path) -> io::Result<()> {
        self.inner.filesystem(tag, path, 0, true)
    }
    fn managed_filesystem(&mut self, tag: &str, path: &Path, read_only: bool) -> io::Result<()> {
        Err(unsupported("managed_filesystem"))
    }
    fn immutable_filesystem(
        &mut self,
        tag: &str,
        path: &Path,
        dax_window_size: usize,
    ) -> io::Result<()> {
        Err(unsupported("immutable_filesystem"))
    }
    fn overlay(&mut self, tag: &str, overlay: OverlayConfig, shm_size: usize) -> io::Result<()> {
        Err(unsupported("overlay"))
    }
    fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: impl Into<Arc<[u8]>>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()> {
        self.inner
            .virtual_file(tag, path, data.into(), mode, one_shot)
    }
    fn network(&mut self, stream: impl Into<NetworkStream>, mac: [u8; 6]) -> io::Result<()> {
        Err(unsupported("network"))
    }
    fn task_stdio(&mut self, stdin: i32, stdout: i32, stderr: i32) -> io::Result<()> {
        Err(unsupported("task_stdio"))
    }
    fn network_with_options(
        &mut self,
        stream: impl Into<NetworkStream>,
        mac: [u8; 6],
        options: NetworkOptions,
    ) -> io::Result<()> {
        Err(unsupported("network_with_options"))
    }
    fn vsock_port(&mut self, guest_port: u32, host_path: PathBuf, listen: bool) -> io::Result<()> {
        Err(unsupported("vsock_port"))
    }
    fn snapshot_profile(&mut self) -> io::Result<()> {
        Err(unsupported("snapshot_profile"))
    }
    fn machine_restore(&mut self, restore: MachineRestore) -> io::Result<()> {
        Err(unsupported("machine_restore"))
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
        Err(unsupported("with_snapshot_quiesced").to_string())
    }
    fn with_snapshot_frozen<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        Err(unsupported("with_snapshot_frozen").to_string())
    }
}

impl ColdRamControl for VmmHandle {
    fn start_cold_pager<S: ColdRamStore + 'static>(
        &self,
        store: S,
        options: ColdRamOptions,
    ) -> io::Result<()> {
        Err(unsupported("start_cold_pager"))
    }
    fn with_ram_quiesced<T>(
        &self,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        Err(unsupported("with_ram_quiesced").to_string())
    }
    fn experimental_ram_residency(&self) -> Result<Option<u64>, String> {
        Err(unsupported("experimental_ram_residency").to_string())
    }
    fn install_ram_fault_handler(&self, handler: Arc<MemoryFaultHandler>) -> Result<(), String> {
        Err(unsupported("install_ram_fault_handler").to_string())
    }
}

impl SnapshotCapture for FrozenMachine<'_> {
    fn capture_machine_state(&self, file: &File) -> Result<MachineSnapshot, String> {
        Err(unsupported("capture_machine_state").to_string())
    }
    fn capture_machine_state_with_ram_delta(
        &self,
        file: &File,
        baseline: Option<&RamDeltaSpec>,
    ) -> Result<(MachineSnapshot, Option<RamDeltaCapture>), String> {
        Err(unsupported("capture_machine_state_with_ram_delta").to_string())
    }
}

impl FrozenMemory for FrozenMachine<'_> {
    fn experimental_ram_blocks(&self, bytes: usize) -> Result<Vec<RamBlock>, String> {
        Err(unsupported("experimental_ram_blocks").to_string())
    }
    fn experimental_ram_page_inventory(&self) -> Result<(u64, Vec<[u64; 4]>), String> {
        Err(unsupported("experimental_ram_page_inventory").to_string())
    }
    fn install_memory_fault_handler(
        &mut self,
        handler: Arc<MemoryFaultHandler>,
    ) -> Result<(), String> {
        Err(unsupported("install_memory_fault_handler").to_string())
    }
    fn set_device_prepare(&self, handler: Option<Arc<MemoryPrepare>>) -> Result<(), String> {
        Err(unsupported("set_device_prepare").to_string())
    }
}

impl SnapshotState for MachineSnapshot {
    fn cpu_count(&self) -> io::Result<usize> {
        Err(unsupported("cpu_count"))
    }
    fn has_kernel_layout(&self) -> io::Result<bool> {
        Err(unsupported("has_kernel_layout"))
    }
    fn verify_frozen_filesystem_backing(&self, tag: &str) -> io::Result<usize> {
        Err(unsupported("verify_frozen_filesystem_backing"))
    }
    fn rebind_filesystem_lower_copies(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_lower_copies"))
    }
    fn rebind_filesystem_shared_lowers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        shared_lowers: &[PathBuf],
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_shared_lowers"))
    }
    fn rebind_filesystem_policy(
        &mut self,
        tag: &str,
        policy: &pvisor_core::overlay::FileAccessPolicy,
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_policy"))
    }
    fn rebind_filesystem_exclusions(
        &mut self,
        tag: &str,
        excluded_paths: &[PathBuf],
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_exclusions"))
    }
    fn ram_mappings(&self) -> io::Result<Vec<RamMappingSnapshot>> {
        Err(unsupported("ram_mappings"))
    }
    fn rebind_filesystem_copy(
        &mut self,
        tag: &str,
        source: &Path,
        destination: &Path,
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_copy"))
    }
    fn rebind_filesystem_layers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_layers"))
    }
    fn rebind_filesystem_stage(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        immutable_lowers: &[PathBuf],
    ) -> io::Result<usize> {
        Err(unsupported("rebind_filesystem_stage"))
    }
}

impl RestoreState for MachineRestore {
    fn validate(&self, cpu_count: usize) -> Result<(), String> {
        Err(unsupported("validate").to_string())
    }
    fn map_ram(
        &self,
        ranges: &[(vm_memory::GuestAddress, usize)],
    ) -> Result<vm_memory::GuestMemoryMmap, String> {
        Err(unsupported("map_ram").to_string())
    }
}

impl RamFileMapping for RamFileMount {
    fn mount(
        store: Arc<std::sync::Mutex<dyn RamFileStore>>,
        directory: &Path,
    ) -> io::Result<(Self, File)> {
        Err(unsupported("mount"))
    }
}

impl RamAccess for RamBlock {
    fn guest_address(&self) -> u64 {
        match self.inner {}
    }
    fn length(&self) -> usize {
        match self.inner {}
    }
    unsafe fn snapshot(&self, output: &mut [u8]) -> io::Result<()> {
        Err(unsupported("snapshot"))
    }
    unsafe fn observe(&self, enabled: bool) -> Result<(), String> {
        Err(unsupported("observe").to_string())
    }
    unsafe fn discard(&self) -> Result<(), String> {
        Err(unsupported("discard").to_string())
    }
    unsafe fn reclaim_file(&self) -> Result<(), String> {
        Err(unsupported("reclaim_file").to_string())
    }
    unsafe fn restore(&self, input: &[u8]) -> Result<(), String> {
        Err(unsupported("restore").to_string())
    }
}

impl VcpuObservationControl for VmmHandle {
    fn set_vcpu_observation(&self, enabled: bool) -> Result<(), String> {
        Err(unsupported("set_vcpu_observation").to_string())
    }
    fn vcpu_observation(&self) -> Result<VcpuObservationSnapshot, String> {
        Err(unsupported("vcpu_observation").to_string())
    }
}

impl RamDedupControl for VmmHandle {
    fn advise_ram_dedup(&self) -> Result<RamDedupReport, String> {
        Err(unsupported("advise_ram_dedup").to_string())
    }
}

impl GuestIdleControl for VmmHandle {
    fn arm_guest_idle(&self, maximum: Duration) -> Result<u64, String> {
        Err(unsupported("arm_guest_idle").to_string())
    }
    fn resume_and_arm_guest_idle(&self, epoch: u64, maximum: Duration) -> Result<u64, String> {
        Err(unsupported("resume_and_arm_guest_idle").to_string())
    }
    fn guest_idle_status(&self) -> GuestIdleStatus {
        disabled_idle_status()
    }
    fn wait_guest_idle(&self, sequence: u64, timeout: Duration) -> GuestIdleStatus {
        disabled_idle_status()
    }
    fn pause_guest_idle(&self, epoch: u64) -> Result<bool, String> {
        Err(unsupported("pause_guest_idle").to_string())
    }
    fn offload_guest_idle(&self, epoch: u64) -> Result<Option<RamReclaim>, String> {
        Err(unsupported("offload_guest_idle").to_string())
    }
    fn retire_guest_idle(&self, epoch: u64) {}
}

impl RuntimeSupport for VmPlatform {
    fn capabilities() -> Capabilities {
        Capabilities {
            architecture: Architecture::X86_64,
            hypervisor: Hypervisor::Whp,
            machine_snapshot: false,
            cold_ram_faults: false,
        }
    }
    fn init_logging(filter: &str) {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
            .try_init();
    }
    fn embedded_kernel() -> Option<KernelImage> {
        crate::firmware::embedded_kernel()
    }
    fn cold_ram_activity() -> ColdRamActivity {
        ColdRamActivity::default()
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
}
impl VmControl for VmmHandle {
    fn is_paused(&self) -> Result<bool, String> {
        let vm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let vm = vm.lock().map_err(|_| "poisoned VMM")?;
        vm.is_paused()
    }
    fn pause(&self) -> Result<(), String> {
        self.transition(true)
    }
    fn resume(&self) -> Result<(), String> {
        self.transition(false)
    }
    fn offload_ram(&self) -> Result<RamReclaim, String> {
        Err(unsupported("RAM offload").to_string())
    }
}
impl VmmHandle {
    fn transition(&self, paused: bool) -> Result<(), String> {
        let _transition = self.transition.lock().map_err(|_| "poisoned transition")?;
        let vm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let mut vm = vm.lock().map_err(|_| "poisoned VMM")?;
        vm.set_paused(paused)
    }
}

fn disabled_idle_status() -> GuestIdleStatus {
    GuestIdleStatus {
        offer_sequence: None,
        epoch: 0,
        sequence: 0,
        enabled: false,
        offered: false,
        claimed: false,
        remaining_ns: 0,
        wake_reason: None,
        pending_host_reads: 0,
        offload_minimum_ns: 0,
        offload_deadline_skips: 0,
    }
}

//! Compile-time architecture and hypervisor capabilities.
use crate::{
    arch::ArchMemoryInfo,
    polly::event_manager::EventManager,
    utils::eventfd::EventFd,
    vmm::{self, resources::VmResources},
};
use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use vm_memory::GuestAddress;

/// Inputs shared by both architecture layouts. An embedded x86 kernel owns a
/// distinct mapping; ARM boot copies its payload into the main RAM region.
#[derive(Default)]
pub struct MemoryLayout {
    pub bytes: usize,
    pub kernel: Option<(u64, usize)>,
    pub initrd_bytes: u64,
    pub firmware_bytes: Option<usize>,
}

pub trait Architecture: sealed::Architecture {
    const NAME: &'static str;
    const CMDLINE_MAX_SIZE: usize;
    fn memory_regions(layout: MemoryLayout) -> (ArchMemoryInfo, Vec<(GuestAddress, usize)>);
}

#[cfg(target_arch = "aarch64")]
pub struct Aarch64;
#[cfg(target_arch = "aarch64")]
impl sealed::Architecture for Aarch64 {}
#[cfg(target_arch = "aarch64")]
impl Architecture for Aarch64 {
    const NAME: &'static str = "aarch64";
    const CMDLINE_MAX_SIZE: usize = crate::arch::aarch64::layout::CMDLINE_MAX_SIZE;
    fn memory_regions(layout: MemoryLayout) -> (ArchMemoryInfo, Vec<(GuestAddress, usize)>) {
        // ARM copies the kernel into main RAM; no separate kernel hole.
        let _ = layout.kernel;
        crate::arch::aarch64::arch_memory_regions(
            layout.bytes,
            layout.initrd_bytes,
            layout.firmware_bytes,
        )
    }
}
#[cfg(target_arch = "x86_64")]
pub struct X86_64;
#[cfg(target_arch = "x86_64")]
impl sealed::Architecture for X86_64 {}
#[cfg(target_arch = "x86_64")]
impl Architecture for X86_64 {
    const NAME: &'static str = "x86_64";
    const CMDLINE_MAX_SIZE: usize = crate::arch::x86_64::layout::CMDLINE_MAX_SIZE;
    fn memory_regions(layout: MemoryLayout) -> (ArchMemoryInfo, Vec<(GuestAddress, usize)>) {
        let (address, size) = layout.kernel.map_or((None, 0), |(a, s)| (Some(a), s));
        crate::arch::x86_64::arch_memory_regions(
            layout.bytes,
            address,
            size,
            layout.initrd_bytes,
            layout.firmware_bytes,
        )
    }
}
#[cfg(target_arch = "riscv64")]
pub struct Riscv64;
#[cfg(target_arch = "riscv64")]
impl sealed::Architecture for Riscv64 {}
#[cfg(target_arch = "riscv64")]
impl Architecture for Riscv64 {
    const NAME: &'static str = "riscv64";
    const CMDLINE_MAX_SIZE: usize = crate::arch::riscv64::layout::CMDLINE_MAX_SIZE;
    fn memory_regions(layout: MemoryLayout) -> (ArchMemoryInfo, Vec<(GuestAddress, usize)>) {
        crate::arch::riscv64::arch_memory_regions(
            layout.bytes,
            layout.initrd_bytes,
            layout.firmware_bytes,
        )
    }
}
#[cfg(target_arch = "aarch64")]
pub type NativeArchitecture = Aarch64;
#[cfg(target_arch = "x86_64")]
pub type NativeArchitecture = X86_64;
#[cfg(target_arch = "riscv64")]
pub type NativeArchitecture = Riscv64;

/// Host platform capability. Implementations own platform worker requirements;
/// common device attachment and lifecycle stay in the shared VMM builder.
pub trait Backend: sealed::Backend {
    type Arch: Architecture;
    const NAME: &'static str;
    fn build(
        resources: &VmResources,
        events: &mut EventManager,
        shutdown: Option<EventFd>,
        sender: Sender<crate::utils::worker_message::WorkerMessage>,
    ) -> Result<Arc<Mutex<vmm::Vmm>>, vmm::builder::StartMicrovmError> {
        vmm::builder::build_microvm_for_arch::<Self::Arch>(resources, events, shutdown, sender)
    }
    fn start_worker(
        resources: &VmResources,
        vm: Arc<Mutex<vmm::Vmm>>,
        receiver: Receiver<crate::utils::worker_message::WorkerMessage>,
    ) -> std::io::Result<()>;
}

#[cfg(target_os = "linux")]
pub struct Kvm;
#[cfg(target_os = "linux")]
impl sealed::Backend for Kvm {}
#[cfg(target_os = "linux")]
impl Backend for Kvm {
    type Arch = NativeArchitecture;
    const NAME: &'static str = "kvm";
    fn start_worker(
        resources: &VmResources,
        vm: Arc<Mutex<vmm::Vmm>>,
        receiver: Receiver<crate::utils::worker_message::WorkerMessage>,
    ) -> std::io::Result<()> {
        if resources.split_irqchip || cfg!(any(feature = "amd-sev", feature = "tdx")) {
            vmm::worker::start_worker_thread(vm, receiver)
                .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        }
        Ok(())
    }
}
#[cfg(target_os = "macos")]
pub struct Hvf;
#[cfg(target_os = "macos")]
impl sealed::Backend for Hvf {}
#[cfg(target_os = "macos")]
impl Backend for Hvf {
    type Arch = NativeArchitecture;
    const NAME: &'static str = "hvf";
    fn start_worker(
        resources: &VmResources,
        vm: Arc<Mutex<vmm::Vmm>>,
        receiver: Receiver<crate::utils::worker_message::WorkerMessage>,
    ) -> std::io::Result<()> {
        if resources.gpu_virgl_flags.is_some() {
            vmm::worker::start_worker_thread(vm, receiver)
                .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub type NativeBackend = Kvm;
#[cfg(target_os = "macos")]
pub type NativeBackend = Hvf;

/// Only backend/architecture combinations with a complete machine-state
/// implementation can expose snapshot configuration in the typed builder.
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
pub trait SnapshotBackend: Backend {}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl SnapshotBackend for Hvf {}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl SnapshotBackend for Kvm {}

mod sealed {
    pub trait Architecture {}
    pub trait Backend {}
}

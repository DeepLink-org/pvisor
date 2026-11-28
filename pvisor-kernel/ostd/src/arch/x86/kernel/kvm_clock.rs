// SPDX-License-Identifier: MPL-2.0

use core::sync::atomic::{Ordering, fence};

use spin::Once;
use x86::msr;

use crate::{
    arch::{cpu::cpuid, io::io_mem::read_once},
    impl_frame_meta_for,
    mm::{self, Frame, FrameAllocOptions, HasPaddr},
};

// KVM feature bits and MSR values.
// Reference: <https://elixir.bootlin.com/linux/v7.0/source/arch/x86/include/uapi/asm/kvm_para.h>.
const KVM_FEATURE_CLOCKSOURCE2: u32 = 3;
const KVM_FEATURE_STEAL_TIME: u32 = 5;
const MSR_KVM_SYSTEM_TIME_NEW: u32 = 0x4b56_4d01;
const MSR_KVM_STEAL_TIME: u32 = 0x4b56_4d03;
const KVM_MSR_ENABLE_BIT: u64 = 1;

struct StealTimePageMeta;
impl_frame_meta_for!(StealTimePageMeta);

// The ABI requires 64-byte alignment. A dedicated zeroed frame also keeps
// KVM's writes out of ordinary guest memory accessors.
#[repr(C, align(64))]
struct StealTime {
    steal: u64,
    version: u32,
    _reserved: [u8; 52],
}

pub(in crate::arch) struct RuntimeClock {
    page: Frame<StealTimePageMeta>,
    multiplier: u64,
}

impl RuntimeClock {
    pub(in crate::arch) fn setup() -> Option<Self> {
        if !cpuid::query_if_running_under_kvm() || !cpuid::query_kvm_feature(KVM_FEATURE_STEAL_TIME)
        {
            return None;
        }
        let page = FrameAllocOptions::new()
            .alloc_frame_with(StealTimePageMeta)
            .ok()?;
        let multiplier = ((1_000_000_000u128 << 32) / u128::from(crate::arch::tsc_freq())) as u64;
        // SAFETY: KVM advertises the MSR, and the caller retains this aligned
        // frame for the remaining lifetime of the vCPU.
        unsafe { msr::wrmsr(MSR_KVM_STEAL_TIME, page.paddr() as u64 | KVM_MSR_ENABLE_BIT) };
        Some(Self { page, multiplier })
    }

    pub(in crate::arch) fn read_ns(&self) -> u64 {
        let ptr = mm::paddr_to_vaddr(self.page.paddr()) as *const StealTime;
        loop {
            let before: u32;
            let after: u32;
            let steal: u64;
            let low: u32;
            let high: u32;
            // SAFETY: ptr points to a live, aligned KVM frame. x86 orders the
            // three loads. KVM publishes this vCPU's steal delta on re-entry;
            // an unretired RDTSC cannot carry a sample across that boundary.
            // Within one run the TSC read need not serialize nearby instructions.
            unsafe {
                core::arch::asm!(
                    "mov {before:e}, [{ptr} + {version}]",
                    "rdtsc",
                    "mov {steal}, [{ptr}]",
                    "mov {after:e}, [{ptr} + {version}]",
                    ptr = in(reg) ptr,
                    version = const core::mem::offset_of!(StealTime, version),
                    before = out(reg) before,
                    steal = out(reg) steal,
                    after = out(reg) after,
                    out("eax") low,
                    out("edx") high,
                    options(nostack, readonly, preserves_flags),
                );
            }
            if before == after && before & 1 == 0 {
                let tsc = (u64::from(high) << 32) | u64::from(low);
                let ns = (u128::from(tsc) * u128::from(self.multiplier)) >> 32;
                return (ns as u64).saturating_sub(steal);
            }
            core::hint::spin_loop();
        }
    }
}

/// Metadata for the KVM pvclock frame.
///
/// The frame is written by the hypervisor and read by the guest clock code,
/// so it is kept typed to prevent OSTD users from reading or writing it as
/// untyped memory via [`VmReader`] and [`VmWriter`].
///
/// [`VmReader`]: crate::mm::VmReader
/// [`VmWriter`]: crate::mm::VmWriter
struct PvclockPageMeta;
impl_frame_meta_for!(PvclockPageMeta);

/// Guest-visible layout of the KVM pvclock page.
///
/// This is Linux's `struct pvclock_vcpu_time_info`. KVM writes this structure
/// directly into the page we hand it via `MSR_KVM_SYSTEM_TIME_NEW`.
///
/// Reference: <https://elixir.bootlin.com/linux/v7.0/source/arch/x86/include/asm/pvclock-abi.h#L26>.
#[repr(C, align(4))]
struct PvclockVcpuTimeInfo {
    version: u32,
    _pad0: u32,
    _tsc_timestamp: u64,
    _system_time: u64,
    tsc_to_system_mul: u32,
    tsc_shift: i8,
    _flags: u8,
    _pad: [u8; 2],
}

/// A handle to the KVM pvclock page shared with the hypervisor.
struct PvclockPage(Frame<PvclockPageMeta>);

impl PvclockPage {
    /// Sets up the KVM pvclock page and returns a handle to it.
    fn setup() -> Option<&'static Self> {
        static PVCLOCK_PAGE: Once<PvclockPage> = Once::new();

        if !has_kvm_clocksource2() {
            return None;
        }

        if let Some(page) = PVCLOCK_PAGE.get() {
            return Some(page);
        }

        let frame = FrameAllocOptions::new()
            .alloc_frame_with(PvclockPageMeta)
            .ok()?;
        Some(PVCLOCK_PAGE.call_once(|| {
            // SAFETY: `frame` is a live page that will be retained by
            // `PVCLOCK_PAGE`, and this MSR is supported per
            // `has_kvm_clocksource2()` above.
            unsafe {
                msr::wrmsr(
                    MSR_KVM_SYSTEM_TIME_NEW,
                    frame.paddr() as u64 | KVM_MSR_ENABLE_BIT,
                )
            };
            Self(frame)
        }))
    }

    /// Returns a consistent snapshot read from the KVM pvclock page.
    fn read_time_snapshot(&self) -> Option<PvclockTimeSnapshot> {
        const MAX_RETRIES: usize = 1_000_000;

        for _ in 0..MAX_RETRIES {
            // KVM marks an in-progress pvclock update with an odd `version`.
            // Accept fields only when `version` is even and unchanged across the read.
            // Reference: <https://elixir.bootlin.com/linux/v7.0/source/Documentation/virt/kvm/x86/msr.rst#L86-L90>.
            let version_before = self.read_version();
            if version_before & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }

            // FIXME: All synchronization below is based on the assumption that we are performing
            // atomic operations under the same memory model as the KVM side. However:
            // 1. The Linux memory model is incompatible with Rust's.
            // 2. We use non-atomic volatile operations to safely communicate with untrusted
            //    parties. But these operations cannot establish synchronization.

            // Synchronize with the (even) version update so that we can see the data written before
            // the version is written.
            fence(Ordering::Acquire);

            let tsc_to_system_mul = self.read_tsc_to_system_mul();
            let tsc_shift = self.read_tsc_shift();

            // Synchronize with the data update so that we can see the (odd) version update before
            // the data is written (if the data is changed concurrently).
            fence(Ordering::Acquire);

            let version_after = self.read_version();
            if version_before == version_after && tsc_to_system_mul != 0 {
                return Some(PvclockTimeSnapshot {
                    tsc_to_system_mul,
                    tsc_shift,
                });
            }

            core::hint::spin_loop();
        }

        None
    }

    fn read_version(&self) -> u32 {
        // SAFETY: `self.as_ptr()` points to a live KVM pvclock page.
        unsafe { read_once(core::ptr::addr_of!((*self.as_ptr()).version)) }
    }

    fn read_tsc_to_system_mul(&self) -> u32 {
        // SAFETY: `self.as_ptr()` points to a live KVM pvclock page.
        unsafe { read_once(core::ptr::addr_of!((*self.as_ptr()).tsc_to_system_mul)) }
    }

    fn read_tsc_shift(&self) -> i8 {
        // SAFETY: `self.as_ptr()` points to a live KVM pvclock page.
        unsafe { read_once(core::ptr::addr_of!((*self.as_ptr()).tsc_shift)) }
    }

    /// Returns a pointer that points to the pvclock page.
    fn as_ptr(&self) -> *const PvclockVcpuTimeInfo {
        mm::paddr_to_vaddr(self.0.paddr()) as *const PvclockVcpuTimeInfo
    }
}

/// A consistent snapshot read from the KVM pvclock page.
struct PvclockTimeSnapshot {
    tsc_to_system_mul: u32,
    tsc_shift: i8,
}

impl PvclockTimeSnapshot {
    /// Returns the TSC frequency in Hz derived from this snapshot.
    fn tsc_freq_hz(&self) -> Option<u64> {
        // The pvclock ABI encodes the TSC frequency using a scale factor
        // (`tsc_to_system_mul`) and a shift (`tsc_shift`) instead of a direct
        // frequency.
        // Reference: <https://elixir.bootlin.com/linux/v7.0/source/arch/x86/kernel/pvclock.c#L27>.

        let base_khz = (1_000_000u64 << 32).checked_div(u64::from(self.tsc_to_system_mul))?;

        let tsc_khz = if self.tsc_shift < 0 {
            base_khz.checked_shl((self.tsc_shift as i32).unsigned_abs())?
        } else {
            base_khz.checked_shr(self.tsc_shift as u32)?
        };

        let freq = tsc_khz.checked_mul(1000)?;
        (freq != 0).then_some(freq)
    }
}

/// Determines the TSC frequency from KVM's paravirtual clock.
pub(super) fn determine_tsc_freq() -> Option<u64> {
    let pvclock_page = PvclockPage::setup()?;
    let time_snapshot = pvclock_page.read_time_snapshot()?;
    time_snapshot.tsc_freq_hz()
}

fn has_kvm_clocksource2() -> bool {
    cpuid::query_if_running_under_kvm() && cpuid::query_kvm_feature(KVM_FEATURE_CLOCKSOURCE2)
}

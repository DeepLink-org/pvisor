#[cfg_attr(unix, path = "arch/mod.rs")]
#[cfg_attr(windows, path = "windows/arch/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod arch;
#[path = "arch_gen/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod arch_gen;
#[cfg(all(unix, target_arch = "x86_64"))]
#[path = "cpuid/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod cpuid;
#[cfg_attr(unix, path = "devices/mod.rs")]
#[cfg_attr(windows, path = "windows/devices/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod devices;
#[cfg(target_os = "macos")]
#[path = "hvf/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod hvf;
#[cfg_attr(unix, path = "kernel/mod.rs")]
#[cfg_attr(windows, path = "windows/kernel/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod kernel;
#[cfg_attr(unix, path = "polly/mod.rs")]
#[cfg_attr(windows, path = "windows/polly/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod polly;
#[path = "smbios/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod smbios;
#[cfg_attr(unix, path = "utils/mod.rs")]
#[cfg_attr(windows, path = "windows/utils/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod utils;
#[cfg_attr(windows, path = "windows/vmm/mod.rs")]
#[cfg_attr(unix, path = "vmm/mod.rs")]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod vmm;

#[path = "api.rs"]
pub mod api;
#[cfg(unix)]
#[path = "backend.rs"]
mod backend;
#[cfg(unix)]
#[path = "builder.rs"]
mod builder;
#[path = "firmware.rs"]
mod firmware;
#[cfg(unix)]
#[path = "guest_idle.rs"]
mod guest_idle;
#[cfg(unix)]
#[path = "handle.rs"]
mod handle;
#[cfg(unix)]
#[path = "memory.rs"]
mod memory;
#[cfg(unix)]
#[path = "portable.rs"]
mod portable;
#[cfg(unix)]
#[path = "ram_dedup.rs"]
mod ram_dedup;
#[cfg(unix)]
#[path = "shared_kernel.rs"]
mod shared_kernel;
#[path = "startup_profile.rs"]
mod startup_profile;
#[cfg(unix)]
#[path = "vcpu_observation.rs"]
mod vcpu_observation;

#[cfg(test)]
#[path = "kernel_bundle.rs"]
mod kernel_bundle;

#[path = "firmware_store.rs"]
mod firmware_store;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "cold_ram.rs"]
mod cold_ram;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "cold_ram_linux.rs"]
mod cold_ram_linux;

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod cold_ram_scan;

#[cfg(unix)]
#[path = "ram_file.rs"]
mod ram_file;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod shared_ram_macos;

mod transport;

#[cfg(windows)]
#[path = "windows/builder.rs"]
mod builder;

#[cfg(windows)]
#[path = "windows/guest_idle.rs"]
mod guest_idle;

#[cfg(windows)]
#[path = "windows/memory.rs"]
mod memory;

#[cfg(windows)]
#[path = "windows/portable.rs"]
mod portable;

#[cfg(windows)]
#[path = "windows/ram_file.rs"]
mod ram_file;

mod ram_delta;

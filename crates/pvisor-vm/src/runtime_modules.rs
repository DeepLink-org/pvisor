#[path = "arch/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod arch;
#[path = "arch_gen/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod arch_gen;
#[cfg(target_arch = "x86_64")]
#[path = "cpuid/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod cpuid;
#[path = "devices/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod devices;
#[cfg(target_os = "macos")]
#[path = "hvf/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod hvf;
#[path = "kernel/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod kernel;
#[path = "polly/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod polly;
#[path = "smbios/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod smbios;
#[path = "utils/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod utils;
#[path = "vmm/mod.rs"]
#[allow(dead_code, unused_imports)]
// Imported backend internals; hardware/feature paths remain private.
mod vmm;

#[path = "api.rs"]
pub mod api;
#[path = "backend.rs"]
mod backend;
#[path = "builder.rs"]
mod builder;
#[path = "firmware.rs"]
mod firmware;
#[path = "handle.rs"]
mod handle;
#[path = "memory.rs"]
mod memory;
#[path = "portable.rs"]
mod portable;
#[path = "ram_dedup.rs"]
mod ram_dedup;

#[cfg(test)]
#[path = "kernel_bundle.rs"]
mod kernel_bundle;

#[path = "firmware_store.rs"]
mod firmware_store;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "cold_ram.rs"]
mod cold_ram;

#[path = "ram_file.rs"]
mod ram_file;

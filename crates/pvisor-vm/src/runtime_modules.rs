#[path = "arch/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod arch;
#[path = "arch_gen/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod arch_gen;
#[cfg(target_arch = "x86_64")]
#[path = "cpuid/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod cpuid;
#[path = "devices/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod devices;
#[cfg(target_os = "macos")]
#[path = "hvf/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod hvf;
#[path = "kernel/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod kernel;
#[path = "polly/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod polly;
#[path = "smbios/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod smbios;
#[path = "utils/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod utils;
#[path = "vmm/mod.rs"]
#[allow(dead_code, unused_imports)] // Imported backend internals; hardware/feature paths remain private.
mod vmm;

#[path = "backend.rs"]
mod backend;
#[path = "builder.rs"]
mod builder;
#[path = "firmware.rs"]
mod firmware;
#[path = "handle.rs"]
mod handle;
#[path = "api.rs"]
pub mod api;
#[path = "portable.rs"]
mod portable;
#[path = "memory.rs"]
mod memory;

#[cfg(test)]
#[path = "kernel_bundle.rs"]
mod kernel_bundle;

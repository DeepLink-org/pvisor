//! Exercise the VMM's RAM selection and fallbacks without requiring KVM.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[allow(dead_code)]
#[path = "../../../vendor/krun-vmm/src/linux/prefault.rs"]
mod prefault;

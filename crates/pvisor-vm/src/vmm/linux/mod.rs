#[cfg(feature = "tee")]
pub mod tee;

pub mod vstate;

#[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
pub(crate) mod prefault;

#[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
pub(crate) mod cpuid;

//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

pub mod apply;
mod core;
pub mod sys;

pub use core::{
    OPAQUE_NAME, OverlayCore, OverlayLayout, Resolved, WHITEOUT_PREFIX, fingerprint_at,
    load_preimages, preimage_journal_is_complete, remove_preimages,
};
pub use pvisor_core::overlay::FileAccessPolicy;

// Preserve the existing import paths; the shared records are owned by Control.
pub use pvisor_core::overlay::{PathFingerprint, PathPreimage};

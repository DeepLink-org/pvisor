//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

pub mod apply;
mod core;
pub mod profile;
pub mod sys;

pub use core::{
    BackingIdentity, BackingResolution, DirectoryEntry, OPAQUE_NAME, OverlayCore, OverlayLayout,
    ROOT_METADATA_NAME, Resolved, ResolvedMetadata, WHITEOUT_PREFIX, fingerprint_at,
    is_opaque_directory, load_preimages, preimage_journal_is_complete, remove_preimages,
    validate_guest_xattr,
};
pub use pvisor_core::overlay::FileAccessPolicy;

// Preserve the existing import paths; the shared records are owned by Control.
pub use pvisor_core::overlay::{PathFingerprint, PathPreimage};

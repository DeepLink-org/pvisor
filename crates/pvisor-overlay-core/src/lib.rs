//! Portable overlay filesystem semantics shared by host FUSE and libkrun virtio-fs.

pub mod apply;
pub mod backend;
mod content_index;
mod core;
pub use content_index::encode_content_index;
pub mod preimage_log;
pub mod profile;
pub mod service;
pub mod stage;
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

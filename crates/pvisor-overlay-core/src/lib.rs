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
    BackingIdentity, BackingResolution, DirectoryEntry, LayerMutability, OPAQUE_NAME,
    OVERRIDE_STAT_XATTR, OverlayCore, OverlayLayout, PRIVATE_BACKING_XATTR, PreparedCopyUp,
    PreparedCopyUps, ROOT_METADATA_NAME, Resolved, ResolvedMetadata,
    STORED_FUSEOVERLAYFS_XATTR_PREFIX, STORED_OVERLAY_XATTR_PREFIX, VIRTUAL_NODE_XATTR,
    WHITEOUT_PREFIX, fingerprint_at, guest_overlay_xattr_name, is_opaque_directory,
    is_private_metadata_xattr, is_stored_overlay_xattr, load_preimages, logical_identity,
    preimage_journal_is_complete, remove_preimages, stored_overlay_xattr_name,
    validate_guest_xattr,
};
pub use pvisor_core::overlay::FileAccessPolicy;

// Shared records are owned by Core.
pub use pvisor_core::overlay::{PathFingerprint, PathPreimage};

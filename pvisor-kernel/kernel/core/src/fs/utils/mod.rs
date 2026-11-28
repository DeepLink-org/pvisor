// SPDX-License-Identifier: MPL-2.0

//! Miscellaneous filesystem utilities shared across `fs` modules.

pub(crate) use dirent_visitor::DirentVisitor;
pub(crate) use direntry_vec::DirEntryVecExt;
pub(crate) use endpoint::{Endpoint, EndpointState};

mod dirent_visitor;
mod direntry_vec;
mod endpoint;
pub(crate) mod systree_inode;

/// Maximum bytes in a path
pub(crate) const PATH_MAX: usize = 4096;

/// Maximum bytes in a file name
pub(crate) const NAME_MAX: usize = 255;

/// The upper limit for resolving symbolic links
pub(crate) const SYMLINKS_MAX: usize = 40;

pub(crate) type CStr256 = aster_util::fixed_str::FixedCStr<256>;

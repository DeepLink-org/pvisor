//! Capture eligibility filters.
//!
//! Storyline and AgenticMD projection are no longer part of this crate. Canonical
//! events remain the capture contract.

mod policy;

pub mod dialogue;

pub use policy::{should_refresh_frontmatter, should_skip_record};

pub fn skip_markdown_block(rec: &crate::record::EventRecord) -> bool {
    should_skip_record(rec)
}

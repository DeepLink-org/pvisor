//! Capture eligibility filters.
//!
//! Canonical events remain the capture contract.

mod policy;

pub mod dialogue;

pub use policy::{should_refresh_frontmatter, should_skip_record};

pub fn skip_markdown_block(rec: &crate::record::CaptureRecord) -> bool {
    should_skip_record(rec)
}

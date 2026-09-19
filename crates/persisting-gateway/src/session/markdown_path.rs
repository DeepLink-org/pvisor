//! Filesystem-safe session markdown filenames.
//!
//! These helpers only name capture files. They are not a Storyline document model.

use std::path::{Path, PathBuf};

const SESSION_FILENAME_MAX_LEN: usize = 128;

pub fn session_markdown_filename(session_key: &str) -> String {
    format!("{}.md", session_filename_stem(session_key))
}

pub fn session_markdown_write_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    run_dir.join(session_markdown_filename(session_key))
}

pub fn is_subagent_session_storage_key(session_key: &str) -> bool {
    session_key
        .trim()
        .strip_prefix("agent-")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn session_filename_stem(session_id: &str) -> String {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return "session".to_string();
    }

    let mut encoded = String::with_capacity(trimmed.len().min(SESSION_FILENAME_MAX_LEN));
    for byte in trimmed.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(*byte as char);
        } else {
            encoded.push('~');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }

    if encoded.len() <= SESSION_FILENAME_MAX_LEN {
        return encoded;
    }

    let digest = blake3::hash(trimmed.as_bytes()).to_hex();
    let suffix = format!("~h{}", &digest[..16]);
    let prefix_limit = SESSION_FILENAME_MAX_LEN - suffix.len();
    let mut prefix = String::with_capacity(prefix_limit);
    for byte in trimmed.as_bytes() {
        let token_len = if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            1
        } else {
            3
        };
        if prefix.len() + token_len > prefix_limit {
            break;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            prefix.push(*byte as char);
        } else {
            prefix.push('~');
            prefix.push_str(&format!("{byte:02X}"));
        }
    }
    prefix.push_str(&suffix);
    prefix
}

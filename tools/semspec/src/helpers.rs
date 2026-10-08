use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use similar::TextDiff;
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, PermissionsExt},
};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
pub fn unified_diff(before: &str, after: &str) -> String {
    TextDiff::from_lines(before, after)
        .unified_diff()
        .header("approved", "current")
        .to_string()
}
#[cfg(unix)]
fn quote_bytes(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return serde_json::to_string(text).unwrap();
    }
    // Preserve arbitrary Unix filenames without replacement-character collisions.
    let mut out = String::from("\"");
    for byte in bytes {
        out.push_str(&format!("\\u{byte:04x}"));
    }
    out.push('"');
    out
}
#[cfg(unix)]
pub fn tree_state(root: &Path) -> Result<String> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "tree-state requires a real directory"
    );
    fn walk(root: &Path, path: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            let info = fs::symlink_metadata(&path)?;
            let relative = path.strip_prefix(root)?.to_path_buf();
            let name = quote_bytes(relative.as_os_str().as_bytes());
            let mode = info.permissions().mode() & 0o7777;
            let kind = info.file_type();
            let line = if kind.is_symlink() {
                format!(
                    "link {name} -> {}",
                    quote_bytes(fs::read_link(&path)?.as_os_str().as_bytes())
                )
            } else if kind.is_dir() {
                format!("dir {mode:04o} {name}")
            } else if kind.is_file() {
                let mut hash = Sha256::new();
                let mut file = fs::File::open(&path)?;
                io::copy(&mut file, &mut HashWriter(&mut hash))?;
                format!(
                    "file {mode:04o} {name} {}",
                    crate::seal::hex(&hash.finalize())
                )
            } else {
                format!("other {:o} {name}", info.mode() & 0o170000)
            };
            out.push((relative, line));
            if kind.is_dir() {
                walk(root, &path, out)?;
            }
        }
        Ok(())
    }
    let mode = fs::symlink_metadata(root)?.permissions().mode() & 0o7777;
    let mut out = vec![(PathBuf::new(), format!("dir {mode:04o} \".\""))];
    walk(root, root, &mut out)?;
    out.sort_by(|(a, _), (b, _)| a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes()));
    let mut text = out
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    Ok(text)
}
#[cfg(unix)]
struct HashWriter<'a>(&'a mut Sha256);
#[cfg(unix)]
impl io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(not(unix))]
pub fn tree_state(_: &Path) -> Result<String> {
    anyhow::bail!("v0.1 tree-state requires Unix")
}
pub fn json_get(path: &Path, pointer: &str) -> Result<String> {
    ensure!(
        pointer.is_empty() || pointer.starts_with('/'),
        "invalid JSON pointer"
    );
    for token in pointer.split('/').skip(1) {
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            if c == '~' {
                ensure!(
                    matches!(chars.next(), Some('0' | '1')),
                    "invalid JSON pointer escape"
                );
            }
        }
    }
    let value: serde_json::Value = serde_json::from_reader(fs::File::open(path)?)?;
    let value = value.pointer(pointer).context("JSON pointer is absent")?;
    Ok(serde_json::to_string(value)?)
}
pub fn file_diff(a: &Path, b: &Path) -> Result<(String, i32)> {
    fn read(path: &Path) -> Result<String> {
        let mut s = String::new();
        if path == Path::new("-") {
            io::stdin().read_to_string(&mut s)?;
        } else {
            s = fs::read_to_string(path)?;
        }
        Ok(s)
    }
    let a = read(a)?;
    let b = read(b)?;
    Ok((unified_diff(&a, &b), i32::from(a != b)))
}

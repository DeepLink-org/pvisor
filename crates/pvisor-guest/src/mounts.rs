//! Decode the kernel mount table for the final guest-to-host writeback barrier.
use std::{ffi::OsString, io, os::unix::ffi::OsStringExt, path::PathBuf};

pub(crate) fn virtiofs_mounts(bytes: &[u8]) -> io::Result<Vec<PathBuf>> {
    if bytes.len() > 1 << 20 {
        return Err(io::Error::other("guest mount table exceeds 1 MiB"));
    }
    let mut paths = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<_> = line
            .split(u8::is_ascii_whitespace)
            .filter(|f| !f.is_empty())
            .collect();
        if fields.len() != 6 {
            return Err(io::Error::other("invalid guest mount record"));
        }
        if fields[2] != b"virtiofs" {
            continue;
        }
        let mut path = Vec::new();
        let mut rest = fields[1];
        while let Some((&byte, tail)) = rest.split_first() {
            if byte == b'\\' {
                let decoded = match tail.get(..3) {
                    Some(b"040") => b' ',
                    Some(b"011") => b'\t',
                    Some(b"012") => b'\n',
                    Some(b"134") => b'\\',
                    _ => return Err(io::Error::other("invalid guest mount escape")),
                };
                path.push(decoded);
                rest = &tail[3..];
            } else {
                if byte == 0 {
                    return Err(io::Error::other("NUL in guest mount path"));
                }
                path.push(byte);
                rest = tail;
            }
        }
        let path = PathBuf::from(OsString::from_vec(path));
        if !path.is_absolute() {
            return Err(io::Error::other("relative guest mount path"));
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_virtiofs_mounts_and_decodes_paths() {
        let paths = virtiofs_mounts(b"root / virtiofs rw 0 0\nproc /proc proc rw 0 0\nwork /a\\040b\\134c virtiofs rw 0 0\nwork /a\\040b\\134c virtiofs rw 0 0\nraw /x\xff virtiofs rw 0 0\n").unwrap();
        assert_eq!(
            paths,
            [
                PathBuf::from("/"),
                PathBuf::from("/a b\\c"),
                PathBuf::from(OsString::from_vec(b"/x\xff".to_vec()))
            ]
        );
    }

    #[test]
    fn rejects_malformed_or_unbounded_tables() {
        for line in [
            b"root / virtiofs\n".as_slice(),
            b"root relative virtiofs rw 0 0\n",
            b"root /a\\777 virtiofs rw 0 0\n",
            b"root /a\0 virtiofs rw 0 0\n",
        ] {
            assert!(virtiofs_mounts(line).is_err());
        }
        assert!(virtiofs_mounts(&vec![b' '; (1 << 20) + 1]).is_err());
    }
}

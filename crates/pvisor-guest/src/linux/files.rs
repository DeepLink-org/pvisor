//! Guest-local file primitives. No host projection shortcuts or subprocesses.
use pvisor_guest::{
    GuestConfig,
    control::{FileRequest, Reply, StdinFrame, read_frame, write_frame},
};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path},
};

fn path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file path must be absolute and NUL-free",
        ));
    }
    Ok(())
}
fn mode(mode: u32) -> io::Result<fs::Permissions> {
    if mode > 0o7777 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix mode",
        ));
    }
    Ok(fs::Permissions::from_mode(mode))
}
fn account(file: &str, id: u32) -> String {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 3 && fields[2].parse::<u32>().ok() == Some(id))
                .then(|| fields[0].to_owned())
        })
        .unwrap_or_else(|| id.to_string())
}
fn stat(path: &Path) -> io::Result<Value> {
    let link = fs::symlink_metadata(path)?;
    let metadata = fs::metadata(path).or_else(|error| {
        if link.is_symlink() {
            Ok(link.clone())
        } else {
            Err(error)
        }
    })?;
    Ok(
        json!({"path": path, "name": path.file_name().unwrap_or_default().to_string_lossy(),
            "type": if metadata.is_dir() { "dir" } else { "file" },
            "size": metadata.len(), "mode": metadata.mode() & 0o7777,
            "owner": account("/etc/passwd", metadata.uid()), "group": account("/etc/group", metadata.gid()),
            "modified": metadata.mtime(), "changed": metadata.ctime(), "isSymlink": link.is_symlink(),
            "symlinkTarget": if link.is_symlink() { Some(fs::read_link(path)?.to_string_lossy().into_owned()) } else { None }
        }),
    )
}
pub(super) fn serve(stream: &mut impl Duplex, request: FileRequest) -> io::Result<()> {
    let value = match request {
        FileRequest::Read { path: file } => {
            path(&file)?;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(file)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "read requires a regular file",
                ));
            }
            write_frame(stream, &Reply::Ready)?;
            let mut bytes = [0; 16384];
            loop {
                let size = file.read(&mut bytes)?;
                if size == 0 {
                    break;
                }
                write_frame(
                    stream,
                    &Reply::Output {
                        stream: 1,
                        bytes: bytes[..size].to_vec(),
                    },
                )?;
            }
            return write_frame(stream, &Reply::Exit { code: 0 });
        }
        FileRequest::Write {
            path: file,
            mode: bits,
        } => {
            path(&file)?;
            let permission = mode(bits)?;
            // O_NONBLOCK avoids hanging PID-1 handlers on a guest-created FIFO.
            use std::os::unix::fs::OpenOptionsExt;
            let mut output = OpenOptions::new()
                .create(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NONBLOCK)
                .open(&file)?;
            if !output.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write requires a regular file",
                ));
            }
            output.set_len(0)?;
            write_frame(stream, &Reply::Ready)?;
            while let StdinFrame::Data { bytes } = read_frame::<StdinFrame>(stream)? {
                output.write_all(&bytes)?;
            }
            output.set_permissions(permission)?;
            output.sync_all()?;
            stat(&file)?
        }
        FileRequest::Stat { path: file } => {
            path(&file)?;
            stat(&file)?
        }
        FileRequest::List { path: file } => {
            path(&file)?;
            let mut entries = Vec::new();
            for entry in fs::read_dir(file)? {
                if entries.len() == 1024 {
                    return Err(io::Error::other(
                        "directory exceeds native list limit (1024)",
                    ));
                }
                entries.push(stat(&entry?.path())?);
            }
            json!(entries)
        }
        FileRequest::MakeDir {
            path: file,
            mode: bits,
        } => {
            path(&file)?;
            let permission = mode(bits)?;
            fs::create_dir_all(&file)?;
            fs::set_permissions(&file, permission)?;
            stat(&file)?
        }
        FileRequest::Remove { path: file } => {
            path(&file)?;
            if !file
                .components()
                .any(|part| matches!(part, Component::Normal(_)))
                || file
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing root/traversal removal",
                ));
            }
            let metadata = fs::symlink_metadata(&file)?;
            if metadata.is_dir() {
                fs::remove_dir_all(file)?;
            } else {
                fs::remove_file(file)?;
            }
            json!({})
        }
        FileRequest::Move {
            source,
            destination,
        } => {
            path(&source)?;
            path(&destination)?;
            fs::rename(source, &destination)?;
            stat(&destination)?
        }
        FileRequest::Chmod {
            path: file,
            mode: bits,
        } => {
            path(&file)?;
            fs::set_permissions(&file, mode(bits)?)?;
            stat(&file)?
        }
    };
    write_frame(stream, &Reply::Value { value })
}
// Kept generic to exercise streaming without a VM; no test-only public surface.
pub(super) trait Duplex: Read + Write {}
impl<T: Read + Write> Duplex for T {}

pub(super) fn info(config: &GuestConfig) -> io::Result<Value> {
    let mem = fs::read_to_string("/proc/meminfo")?;
    let kb = |key: &str| -> u64 {
        mem.lines()
            .find_map(|line| {
                line.strip_prefix(key)?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    };
    let total = kb("MemTotal:");
    let available = kb("MemAvailable:");
    let cpu = fs::read_to_string("/proc/stat")?;
    let ticks: Vec<u64> = cpu
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|v| v.parse().unwrap_or(0))
        .collect();
    let total_ticks: u64 = ticks.iter().sum();
    let busy_ticks = total_ticks
        .saturating_sub(ticks.get(3).copied().unwrap_or(0))
        .saturating_sub(ticks.get(4).copied().unwrap_or(0));
    Ok(
        json!({"cpu_busy_ticks":busy_ticks,"cpu_total_ticks":total_ticks,"sandboxId": config.env.get("PVISOR_NATIVE_SANDBOX_ID"), "generation": config.env.get("PVISOR_NATIVE_GENERATION"),
        "cpu_count": std::thread::available_parallelism()?.get(), "mem_total_mib": total as f64 / 1024.0,
        "mem_used_mib": total.saturating_sub(available) as f64 / 1024.0}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    #[test]
    fn binary_streams_and_guest_symlink_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bytes");
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let target = file.clone();
        let worker = std::thread::spawn(move || {
            serve(
                &mut server,
                FileRequest::Write {
                    path: target,
                    mode: 0o600,
                },
            )
            .unwrap()
        });
        assert!(matches!(
            read_frame::<Reply>(&mut client).unwrap(),
            Reply::Ready
        ));
        let bytes: Vec<u8> = (0..=255).cycle().take(131073).collect();
        for chunk in bytes.chunks(16384) {
            write_frame(
                &mut client,
                &StdinFrame::Data {
                    bytes: chunk.to_vec(),
                },
            )
            .unwrap();
        }
        write_frame(&mut client, &StdinFrame::Eof {}).unwrap();
        assert!(matches!(
            read_frame::<Reply>(&mut client).unwrap(),
            Reply::Value { .. }
        ));
        worker.join().unwrap();
        assert_eq!(fs::read(&file).unwrap(), bytes);
        let metadata = fs::metadata(&file).unwrap();
        let info = stat(&file).unwrap();
        assert_eq!(info["modified"], metadata.mtime());
        assert_eq!(info["changed"], metadata.ctime());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(stat(&link).unwrap()["isSymlink"], true);
        assert!(path(Path::new("relative")).is_err());
        assert!(mode(0o10000).is_err());
    }
}

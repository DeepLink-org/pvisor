//! Bounded, best-effort Linux metadata reads. Never used for authorization.
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::session::client::SessionClientMeta;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_PIDS: usize = 8192;
const MAX_FDS: usize = 65536;

fn read(path: &Path, limit: usize) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn address(raw: &str) -> Option<SocketAddr> {
    let (hex, port) = raw.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match hex.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(hex, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut bytes = [0; 16];
            for (i, word) in hex.as_bytes().as_chunks::<8>().0.iter().enumerate() {
                let word = u32::from_str_radix(std::str::from_utf8(word).ok()?, 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && a.ip().to_canonical() == b.ip().to_canonical()
}

fn socket_inodes(table: &str, peer: SocketAddr) -> HashSet<u64> {
    table
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 10 || fields[3] != "01" {
                return None;
            }
            let local = address(fields[1])?;
            if !same_endpoint(local, peer) {
                return None;
            }
            fields[9].parse::<u64>().ok().filter(|inode| *inode != 0)
        })
        .collect()
}

// stat comm may contain spaces and ')'; starttime is field 22.
fn start_time(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
pub(super) fn resolve(peer: SocketAddr) -> Option<SessionClientMeta> {
    resolve_at(
        Path::new("/proc"),
        peer,
        std::process::id(),
        Duration::from_secs(1),
    )
}

fn resolve_at(
    root: &Path,
    peer: SocketAddr,
    exclude: u32,
    budget: Duration,
) -> Option<SessionClientMeta> {
    let deadline = Instant::now() + budget;
    let mut inodes = HashSet::new();
    for name in ["net/tcp", "net/tcp6"] {
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(table) = read(&root.join(name), MAX_BYTES) {
            inodes.extend(socket_inodes(&table, peer));
        }
    }
    if inodes.is_empty() {
        return None;
    }
    let mut found = None;
    let mut fd_count = 0;
    for (count, entry) in fs::read_dir(root).ok()?.enumerate() {
        if count >= MAX_PIDS || Instant::now() >= deadline {
            return None;
        }
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == 0 || pid == exclude {
            continue;
        }
        let dir = entry.path();
        let Some(before) = read(&dir.join("stat"), 65536).and_then(|s| start_time(&s)) else {
            continue;
        };
        let Ok(fds) = fs::read_dir(dir.join("fd")) else {
            continue;
        };
        for fd in fds {
            fd_count += 1;
            if fd_count > MAX_FDS || Instant::now() >= deadline {
                return None;
            }
            let Ok(fd) = fd else { continue };
            let Ok(link) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = link
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
            else {
                continue;
            };
            if !inodes.contains(&inode) {
                continue;
            }
            let command = read(&dir.join("cmdline"), 1024 * 1024)?
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if command.is_empty() {
                return None;
            }
            let after = read(&dir.join("stat"), 65536).and_then(|s| start_time(&s));
            if after != Some(before) {
                return None;
            }
            // Inherited/shared sockets cannot identify a unique client process.
            if found.is_some() {
                return None;
            }
            found = Some(SessionClientMeta {
                peer: peer.to_string(),
                peer_port: peer.port(),
                pid,
                command,
                machine_fp: None,
            });
            break;
        }
    }
    if Instant::now() >= deadline {
        None
    } else {
        found
    }
}

#[cfg(target_os = "linux")]
pub(super) fn username(pid: u32) -> Option<String> {
    username_at(Path::new("/proc"), Path::new("/etc/passwd"), pid)
}

fn username_at(root: &Path, passwd: &Path, pid: u32) -> Option<String> {
    let status = read(&root.join(pid.to_string()).join("status"), 65536)?;
    let uid: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    // Local names only: NSS may block on a network service. Numeric UID is a
    // deterministic fallback, rather than incorrectly using the proxy's owner.
    if let Some(passwd) = read(passwd, MAX_BYTES) {
        for line in passwd.lines() {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() >= 3
                && !fields[0].is_empty()
                && fields[2].parse::<u32>().ok() == Some(uid)
            {
                return Some(fields[0].to_owned());
            }
        }
    }
    Some(uid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn row(local: &str, state: &str, inode: u64) -> String {
        format!("0: {local} 0100007F:0050 {state} 0:0 0:0 0 1000 0 {inode}\n")
    }
    fn process(root: &Path, pid: u32, inode: u64) {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        fs::write(
            dir.join("stat"),
            format!("{pid} (some ) name) S {} 42", "0 ".repeat(18)),
        )
        .unwrap();
        fs::write(dir.join("cmdline"), b"python\0client.py\0").unwrap();
        symlink(format!("socket:[{inode}]"), dir.join("fd/3")).unwrap();
    }
    #[test]
    fn ipv4_ipv6_mapped_and_state_parsing() {
        let peer = "127.0.0.1:54321".parse().unwrap();
        assert_eq!(
            socket_inodes(&row("0100007F:D431", "01", 7), peer),
            HashSet::from([7])
        );
        assert!(socket_inodes(&row("0100007F:D431", "0A", 7), peer).is_empty());
        assert!(socket_inodes(&row("0100007F:D432", "01", 7), peer).is_empty());
        assert_eq!(
            socket_inodes(&row("0000000000000000FFFF00000100007F:D431", "01", 8), peer),
            HashSet::from([8])
        );
        assert_eq!(
            address("00000000000000000000000001000000:0050"),
            Some("[::1]:80".parse().unwrap())
        );
        assert!(socket_inodes("broken\n", peer).is_empty());
    }
    #[test]
    fn fixture_lookup_missing_ambiguous_and_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("net")).unwrap();
        fs::write(root.join("net/tcp"), row("0100007F:D431", "01", 7)).unwrap();
        process(root, 123, 7);
        let peer = "127.0.0.1:54321".parse().unwrap();
        let lookup = || resolve_at(root, peer, 0, Duration::from_secs(1));
        let found = lookup().unwrap();
        assert_eq!(found.pid, 123);
        assert_eq!(found.command, "python client.py");
        assert!(resolve_at(root, peer, 0, Duration::ZERO).is_none());
        assert!(resolve_at(root, peer, 123, Duration::from_secs(1)).is_none());
        process(root, 124, 7);
        assert!(lookup().is_none());
        fs::remove_dir_all(root.join("124")).unwrap();
        fs::remove_file(root.join("123/cmdline")).unwrap();
        assert!(lookup().is_none());
    }
    #[test]
    fn effective_uid_local_name_and_numeric_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("123")).unwrap();
        fs::write(root.join("123/status"), "Uid:\t1000\t1001\t1002\t1003\n").unwrap();
        let passwd = root.join("passwd");
        fs::write(
            &passwd,
            "real:x:1000:0::/:/bin/sh\neffective:x:1001:0::/:/bin/sh\n",
        )
        .unwrap();
        assert_eq!(
            username_at(root, &passwd, 123).as_deref(),
            Some("effective")
        );
        fs::remove_file(&passwd).unwrap();
        assert_eq!(username_at(root, &passwd, 123).as_deref(), Some("1001"));
        assert!(username_at(root, &passwd, 999).is_none());
    }
    #[test]
    fn oversized_reads_and_bad_stat_are_rejected() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), "12345").unwrap();
        assert!(read(tmp.path(), 4).is_none());
        assert!(start_time("broken").is_none());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn live_linux_socket_resolves_without_external_commands() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(stream.local_addr().unwrap(), peer);
        let found = resolve_at(Path::new("/proc"), peer, 0, Duration::from_secs(1)).unwrap();
        assert_eq!(found.pid, std::process::id());
    }
}

//! Linux process identity and one bounded smaps walk. This reads no guest RAM.
use anyhow::{Context, ensure};
use pvisor_core::memory::{NativeVmMemory, ProcessMemory, ResidentMemory};
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct Target {
    // Holding the proc directory binds later openat calls to this process even
    // if its numerical PID is recycled. No RAM FD or pager ownership is retained.
    proc: Arc<File>,
    pid: u32,
    start: u64,
    ram_identity: Option<(u64, u64)>,
}
impl Target {
    pub fn new(pid: u32, ram: Option<&File>) -> anyhow::Result<Self> {
        let proc = Arc::new(File::open(format!("/proc/{pid}"))?);
        let start = identity(&read(&proc, c"stat", 64 * 1024)?, pid)?;
        let ram_identity = ram
            .map(|file| file.metadata().map(|m| (m.dev(), m.ino())))
            .transpose()?;
        Ok(Self {
            proc,
            pid,
            start,
            ram_identity,
        })
    }
    pub fn cpu_sample(&self) -> anyhow::Result<pvisor_core::cpu::ProcessCpuUsage> {
        super::cpu::sample(&self.proc, self.pid, self.start)
    }
    pub fn cpu_exit_observer(&self) -> anyhow::Result<super::exit_cpu::Observer> {
        super::exit_cpu::Observer::new(self.proc.clone(), self.pid, self.start)
    }
    pub fn sample(&self) -> anyhow::Result<NativeVmMemory> {
        ensure!(
            identity(&read(&self.proc, c"stat", 64 * 1024)?, self.pid)? == self.start,
            "native VM process identity changed"
        );
        let (device, inode) = self.ram_identity.context(
            "CAPABILITY_UNSUPPORTED: anonymous cold-pager RAM has no file identity for guest/non-RAM attribution"
        )?;
        let text = read(&self.proc, c"smaps", 16 * 1024 * 1024)?;
        let (guest_ram, non_ram) = parse(&text, device, inode)?;
        ensure!(
            identity(&read(&self.proc, c"stat", 64 * 1024)?, self.pid)? == self.start,
            "native VM ended or changed during memory sampling"
        );
        let result = NativeVmMemory {
            pid: self.pid,
            start_time_ticks: self.start,
            process: guest_ram
                .checked_add(&non_ram)
                .context("memory counter overflow")?,
            guest_ram,
            non_ram,
        };
        result.validate()?;
        Ok(result)
    }
}

pub(super) fn read(proc: &File, name: &std::ffi::CStr, limit: u64) -> anyhow::Result<String> {
    let fd = unsafe {
        libc::openat(
            proc.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    ensure!(
        fd >= 0,
        "read native process {}: {}",
        name.to_string_lossy(),
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(fd) };
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= limit,
        "native process observation exceeds limit"
    );
    Ok(text)
}

pub(super) fn identity(stat: &str, pid: u32) -> anyhow::Result<u64> {
    let (head, rest) = stat
        .rsplit_once(')')
        .context("invalid native process stat")?;
    ensure!(
        head.split_whitespace()
            .next()
            .context("missing PID")?
            .parse::<u32>()?
            == pid,
        "native process PID mismatch"
    );
    let fields: Vec<_> = rest.split_whitespace().collect();
    ensure!(
        !matches!(fields.first(), Some(&"Z" | &"X" | &"x")),
        "native VM process ended"
    );
    let start = fields
        .get(19)
        .context("missing native process start time")?
        .parse()?;
    ensure!(start > 0, "invalid native process start time");
    Ok(start)
}

/// Read this supervisor's mappings, including its FUSE service threads. Call
/// from a blocking probe; this holds no execution/control or pager ownership.
pub fn sample_supervisor_memory() -> anyhow::Result<ProcessMemory> {
    let pid = std::process::id();
    let proc = File::open("/proc/self")?;
    let start = identity(&read(&proc, c"stat", 64 * 1024)?, pid)?;
    let (_, process) = parse_mappings(&read(&proc, c"smaps", 16 * 1024 * 1024)?, None)?;
    ensure!(
        identity(&read(&proc, c"stat", 64 * 1024)?, pid)? == start,
        "supervisor identity changed during memory sampling"
    );
    let result = ProcessMemory {
        pid,
        start_time_ticks: start,
        process,
    };
    result.validate()?;
    Ok(result)
}

fn parse(text: &str, device: u64, inode: u64) -> anyhow::Result<(ResidentMemory, ResidentMemory)> {
    let result = parse_mappings(text, Some((device, inode)))?;
    ensure!(
        result.0.mapped_bytes > 0,
        "native guest RAM has not been mapped"
    );
    Ok(result)
}

fn parse_mappings(
    text: &str,
    ram_identity: Option<(u64, u64)>,
) -> anyhow::Result<(ResidentMemory, ResidentMemory)> {
    let mut ram = ResidentMemory::default();
    let mut other = ResidentMemory::default();
    let mut current: Option<(bool, ResidentMemory, u16)> = None;
    let finish = |current: &mut Option<(bool, ResidentMemory, u16)>,
                  ram: &mut ResidentMemory,
                  other: &mut ResidentMemory|
     -> anyhow::Result<()> {
        if let Some((is_ram, counters, fields)) = current.take() {
            ensure!(fields == 0x7ff, "incomplete native smaps mapping");
            counters.validate()?;
            let total = if is_ram { ram } else { other };
            *total = total
                .checked_add(&counters)
                .context("memory counter overflow")?;
        }
        Ok(())
    };
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first().is_some_and(|field| field.contains('-')) {
            finish(&mut current, &mut ram, &mut other)?;
            ensure!(fields.len() >= 5, "invalid native smaps mapping header");
            let (major, minor) = fields[3]
                .split_once(':')
                .context("invalid mapping device")?;
            let major = u64::from_str_radix(major, 16)?;
            let minor = u64::from_str_radix(minor, 16)?;
            let inode = fields[4].parse::<u64>()?;
            let is_ram = ram_identity.is_some_and(|(device, ram_inode)| {
                inode == ram_inode
                    && major == u64::from(libc::major(device))
                    && minor == u64::from(libc::minor(device))
            });
            current = Some((is_ram, ResidentMemory::default(), 0));
        } else if let Some((_, counters, seen)) = &mut current {
            let Some(field) = fields.first() else {
                continue;
            };
            let (index, counter) = match *field {
                "Size:" => (0, &mut counters.mapped_bytes),
                "Rss:" => (1, &mut counters.rss_bytes),
                "Pss:" => (2, &mut counters.pss_bytes),
                "Private_Clean:" => (3, &mut counters.private_clean_bytes),
                "Private_Dirty:" => (4, &mut counters.private_dirty_bytes),
                "Shared_Clean:" => (5, &mut counters.shared_clean_bytes),
                "Shared_Dirty:" => (6, &mut counters.shared_dirty_bytes),
                "Swap:" => (7, &mut counters.swap_bytes),
                "SwapPss:" => (8, &mut counters.swap_pss_bytes),
                "Private_Hugetlb:" => (9, &mut counters.private_hugetlb_bytes),
                "Shared_Hugetlb:" => (10, &mut counters.shared_hugetlb_bytes),
                _ => continue,
            };
            ensure!(
                *seen & (1 << index) == 0 && fields.len() == 3 && fields[2] == "kB",
                "duplicate or invalid native smaps counter"
            );
            *counter = fields[1]
                .parse::<u64>()?
                .checked_mul(1024)
                .context("memory counter overflow")?;
            *seen |= 1 << index;
        } else {
            anyhow::bail!("missing native smaps mapping header");
        }
    }
    finish(&mut current, &mut ram, &mut other)?;
    Ok((ram, other))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_observation_walks_real_mappings_and_preserves_process_identity() {
        let sample = sample_supervisor_memory().unwrap();
        assert_eq!(sample.pid, std::process::id());
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        assert_eq!(
            sample.start_time_ticks,
            identity(&stat, sample.pid).unwrap()
        );
        assert!(sample.process.rss_bytes > 0 && sample.process.pss_bytes > 0);
        let text = mapping("00:42", 7, true) + &mapping("00:42", 7, false);
        let (ram, process) = parse_mappings(&text, None).unwrap();
        assert_eq!(ram, ResidentMemory::default());
        assert_eq!(process.rss_bytes, 16384);
        assert_eq!(process.pss_bytes, 12288);
    }
    fn mapping(device: &str, inode: u64, shared: bool) -> String {
        format!(
            "1000-5000 rw-p 00000000 {device} {inode} /same/path (deleted)\nSize: 16 kB\nRss: 8 kB\nPss: {} kB\nPrivate_Clean: 0 kB\nPrivate_Dirty: {} kB\nShared_Clean: {} kB\nShared_Dirty: 0 kB\nSwap: 0 kB\nSwapPss: 0 kB\nPrivate_Hugetlb: 0 kB\nShared_Hugetlb: 0 kB\nVmFlags: rd wr mr mw\n",
            if shared { 4 } else { 8 },
            if shared { 0 } else { 8 },
            if shared { 8 } else { 0 }
        )
    }
    #[test]
    fn partitions_by_device_and_inode_including_private_cow_and_deleted_names() {
        let text = mapping("00:42", 7, true)
            + &mapping("00:42", 7, false)
            + &mapping("00:43", 7, false)
            + &mapping("00:42", 8, false);
        let (ram, other) = parse(&text, libc::makedev(0, 0x42), 7).unwrap();
        assert_eq!(ram.mapped_bytes, 32 * 1024);
        assert_eq!(ram.pss_bytes, 12 * 1024);
        assert_eq!(ram.private_dirty_bytes, 8 * 1024);
        assert_eq!(ram.shared_clean_bytes, 8 * 1024);
        assert_eq!(other.pss_bytes, 16 * 1024);
        for bad in [
            text.replace("Pss: 4", "Pss: 9"),
            text.replace("Size: 16", "Size: 1"),
            text.replace("SwapPss: 0 kB\n", ""),
            text.replace("Rss: 8 kB", "Rss: 8 kB\nRss: 8 kB"),
            text.replace("Pss: 4 kB", "Pss: 18446744073709551615 kB"),
        ] {
            assert!(parse(&bad, libc::makedev(0, 0x42), 7).is_err());
        }
        assert!(parse(&text, libc::makedev(0, 0x44), 7).is_err());
    }
    #[test]
    fn identity_handles_parentheses_and_rejects_zombies_and_wrong_pid() {
        let stat = format!("123 (name ) with spaces) S {} 99", vec!["0"; 18].join(" "));
        assert_eq!(identity(&stat, 123).unwrap(), 99);
        assert!(identity(&stat, 124).is_err());
        assert!(identity(&stat.replace(") S ", ") Z "), 123).is_err());
        assert!(identity("123 (truncated) S 0", 123).is_err());
    }
    #[test]
    fn retained_proc_directory_never_observes_a_terminated_process_as_zero_memory() {
        let ram = tempfile::tempfile().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let target = Target::new(child.id(), Some(&ram));
        child.kill().unwrap();
        child.wait().unwrap();
        let target = target.unwrap();
        assert!(target.sample().is_err());
        assert!(
            read(&target.proc, c"stat", 64 * 1024).is_err(),
            "held proc directory must remain bound to the exited process"
        );
    }
}

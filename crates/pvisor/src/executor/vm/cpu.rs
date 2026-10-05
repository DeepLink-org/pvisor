//! One bounded, identity-bound proc/stat read; never walks smaps or guest RAM.
use anyhow::{Context, ensure};
use pvisor_core::cpu::ProcessCpuUsage;

pub(super) fn sample(
    proc: &std::fs::File,
    pid: u32,
    start: u64,
) -> anyhow::Result<ProcessCpuUsage> {
    sample_inner(proc, pid, start, false)
}

pub(super) fn sample_exited(
    proc: &std::fs::File,
    pid: u32,
    start: u64,
) -> anyhow::Result<ProcessCpuUsage> {
    sample_inner(proc, pid, start, true)
}

fn sample_inner(
    proc: &std::fs::File,
    pid: u32,
    start: u64,
    exited: bool,
) -> anyhow::Result<ProcessCpuUsage> {
    let text = super::memory::read(proc, c"stat", 64 * 1024)?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    ensure!(hz > 0, "Linux CPU clock tick rate is unavailable");
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } == 0,
        "read monotonic CPU observation clock: {}",
        std::io::Error::last_os_error()
    );
    let monotonic = u64::try_from(clock.tv_sec)?
        .checked_mul(1_000_000_000)
        .and_then(|ns| ns.checked_add(u64::try_from(clock.tv_nsec).ok()?))
        .context("CPU observation clock overflow")?;
    parse_inner(&text, pid, start, u64::try_from(hz)?, monotonic, exited)
}

#[cfg(test)]
fn parse(
    text: &str,
    pid: u32,
    start: u64,
    hz: u64,
    monotonic: u64,
) -> anyhow::Result<ProcessCpuUsage> {
    parse_inner(text, pid, start, hz, monotonic, false)
}

fn parse_inner(
    text: &str,
    pid: u32,
    start: u64,
    hz: u64,
    monotonic: u64,
    exited: bool,
) -> anyhow::Result<ProcessCpuUsage> {
    let (_, rest) = text.rsplit_once(')').context("invalid native CPU stat")?;
    let fields: Vec<_> = rest.split_whitespace().collect();
    let number = |field: usize| -> anyhow::Result<u64> {
        fields
            .get(field - 3)
            .context("truncated native CPU stat")?
            .parse()
            .context("invalid native CPU stat counter")
    };
    if exited {
        let (head, _) = text.split_once('(').context("invalid final CPU stat")?;
        ensure!(
            head.trim().parse::<u32>()? == pid && number(22)? == start,
            "final CPU process identity changed"
        );
        ensure!(
            fields.first() == Some(&"Z"),
            "final CPU process has not exited"
        );
    } else {
        ensure!(
            super::memory::identity(text, pid)? == start,
            "native CPU process identity changed"
        );
    }
    let result = ProcessCpuUsage {
        pid,
        start_time_ticks: start,
        clock_ticks_per_second: hz,
        sampled_monotonic_ns: monotonic,
        threads: u32::try_from(number(20)?)?,
        user_time_ticks: number(14)?,
        system_time_ticks: number(15)?,
        guest_time_ticks: number(43)?,
    };
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stat() -> String {
        let mut fields = vec!["0"; 50];
        fields[0] = "S";
        fields[14 - 3] = "123";
        fields[15 - 3] = "45";
        fields[20 - 3] = "4";
        fields[22 - 3] = "997";
        fields[43 - 3] = "99";
        format!("42 (vm ) tricky )) command) {}", fields.join(" "))
    }
    #[test]
    fn stat_parser_uses_documented_fields_and_handles_parentheses_in_comm() {
        let sample = parse(&stat(), 42, 997, 100, 12345).unwrap();
        assert_eq!(sample.user_time_ticks, 123);
        assert_eq!(sample.system_time_ticks, 45);
        assert_eq!(sample.guest_time_ticks, 99);
        assert_eq!(sample.threads, 4);
        assert_eq!(sample.total_ticks().unwrap(), 168);
        assert!(parse(&stat(), 43, 997, 100, 12345).is_err());
        assert!(parse(&stat(), 42, 998, 100, 12345).is_err());
        assert!(parse(&stat().replace(") S ", ") Z "), 42, 997, 100, 12345).is_err());
        assert!(parse(&stat().replace(" 123 ", " -1 "), 42, 997, 100, 12345).is_err());
        assert!(parse("42 (vm) S 0", 42, 997, 100, 12345).is_err());
    }
    #[test]
    fn cpu_probe_reads_real_thread_group_counters_without_smaps_or_ram() {
        let pid = std::process::id();
        let proc = std::fs::File::open("/proc/self").unwrap();
        let start = super::super::memory::identity(
            &std::fs::read_to_string("/proc/self/stat").unwrap(),
            pid,
        )
        .unwrap();
        let before = sample(&proc, pid, start).unwrap();
        let work = std::thread::spawn(|| {
            let cpu_time = || {
                let mut time: libc::timespec = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
                    0
                );
                time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
            };
            let target = cpu_time() + 30_000_000;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut value = 1_u64;
            while cpu_time() < target {
                assert!(
                    std::time::Instant::now() < deadline,
                    "CPU work must finish within three seconds"
                );
                value = std::hint::black_box(value.wrapping_mul(1664525).wrapping_add(1013904223));
            }
            value
        });
        std::hint::black_box(work.join().unwrap());
        let after = sample(&proc, pid, start).unwrap();
        assert!(after.total_ticks().unwrap() > before.total_ticks().unwrap());
        assert!(after.interval_since(&before).unwrap().total_cpu_time_ns > 0);
        assert!(sample(&proc, pid, start + 1).is_err());
    }
    #[test]
    fn open_proc_descriptor_cannot_follow_an_exited_or_reused_process() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let proc = std::fs::File::open(format!("/proc/{pid}")).unwrap();
        let start = super::super::memory::identity(
            &std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap(),
            pid,
        )
        .unwrap();
        assert!(sample(&proc, pid, start).is_ok());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(sample(&proc, pid, start).is_err());
    }
}

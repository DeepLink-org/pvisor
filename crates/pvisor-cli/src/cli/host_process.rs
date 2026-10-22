//! Request-owned processes and terminal handoff. No cross-request PID signalling.
use anyhow::{Context, ensure};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub(super) fn become_subreaper() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        ensure!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0,
            "establish request orphan ownership: {}",
            std::io::Error::last_os_error()
        );
        let _ = pidfd(std::process::id() as i32)?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        // libproc identities fence numeric signals, but are not pidfds. Track
        // workload groups independently of the worker and freeze discovered
        // forkers before escalation. Unobserved, already-reparented descendants
        // and the identity-check/kill race cannot be contained as on Linux.
        let _ = mac_identity(std::process::id() as i32)?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("host worker ownership is unsupported on this platform")
}
#[cfg(target_os = "linux")]
fn pidfd(pid: i32) -> anyhow::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open request process pidfd");
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
#[cfg(target_os = "linux")]
fn signal(fd: &OwnedFd, sig: i32) -> anyhow::Result<()> {
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn exited(fd: &OwnedFd) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe {
        libc::poll(&mut p, 1, 0);
    }
    p.revents & (libc::POLLIN | libc::POLLHUP) != 0
}
#[cfg(target_os = "linux")]
fn wait_pidfds(mut fds: Vec<libc::pollfd>, deadline: Instant) -> anyhow::Result<()> {
    if fds.is_empty() {
        return Ok(());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = remaining
        .as_millis()
        .saturating_add(1)
        .min(i32::MAX as u128) as i32;
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn process_stopped(pid: i32, fd: &OwnedFd) -> anyhow::Result<bool> {
    if exited(fd) {
        return Ok(true);
    }
    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    // An exit between the pidfd check and /proc read is also quiescent.
    if exited(fd) {
        return Ok(true);
    }
    let state = state?;
    let state = state
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .next()
        .context("missing process state")?;
    if !matches!(state, "T" | "t") {
        return Ok(false);
    }
    // A stopped leader alone is insufficient while another thread is still
    // entering a group stop: that thread may still fork a new descendant.
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task"));
    if exited(fd) {
        return Ok(true);
    }
    for task in tasks? {
        let state = match std::fs::read_to_string(task?.path().join("stat")) {
            Ok(state) => state,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let state = state
            .rsplit_once(')')
            .context("invalid task stat")?
            .1
            .split_whitespace()
            .next()
            .context("missing task state")?;
        if !matches!(state, "T" | "t" | "Z" | "X") {
            return Ok(false);
        }
    }
    Ok(true)
}
#[cfg(target_os = "linux")]
fn stat(pid: i32) -> anyhow::Result<(i32, i32, u64)> {
    let value = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = value
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect();
    Ok((fields[1].parse()?, fields[2].parse()?, fields[19].parse()?))
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {}

#[cfg(target_os = "macos")]
fn mac_identity(pid: i32) -> anyhow::Result<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if n != size {
        return Err(std::io::Error::last_os_error()).context("read owned process identity");
    }
    Ok(info)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy)]
struct MacProcess {
    parent: i32,
    group: i32,
    start: (u64, u64),
    alive: bool,
}

#[cfg(any(target_os = "macos", test))]
fn discover_mac_descendants(
    processes: &mut BTreeMap<i32, (u64, u64)>,
    snapshot: &BTreeMap<i32, MacProcess>,
    parent: i32,
    excluded: i32,
) {
    let mut owned: BTreeSet<i32> = processes
        .iter()
        .filter_map(|(&pid, &start)| {
            snapshot
                .get(&pid)
                .filter(|p| p.start == start && p.alive)
                .map(|_| pid)
        })
        .collect();
    // A live, identity-matched member reserves a known workload group even if
    // its leader was reaped. Never use a stale numeric PID as an ancestry root.
    let groups: BTreeSet<i32> = owned
        .iter()
        .filter_map(|pid| {
            let group = snapshot[pid].group;
            processes.contains_key(&group).then_some(group)
        })
        .collect();
    loop {
        let before = owned.len();
        for (&pid, info) in snapshot {
            if pid == parent || pid == excluded || !info.alive {
                continue;
            }
            if processes
                .get(&pid)
                .is_some_and(|start| *start != info.start)
            {
                continue;
            }
            if owned.contains(&info.parent) || groups.contains(&info.group) {
                owned.insert(pid);
            }
        }
        if before == owned.len() {
            break;
        }
    }
    for pid in owned {
        processes.insert(pid, snapshot[&pid].start);
    }
}

#[cfg(target_os = "macos")]
fn mac_snapshot() -> anyhow::Result<BTreeMap<i32, MacProcess>> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    ensure!(
        (1..=262_000).contains(&count),
        "cannot enumerate macOS request descendants"
    );
    let mut pids = vec![0i32; count as usize + 128];
    let count =
        unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    ensure!(
        count > 0 && (count as usize) < pids.len(),
        "macOS process enumeration truncated; refusing incomplete cleanup"
    );
    let mut snapshot = BTreeMap::new();
    for &pid in &pids[..count as usize] {
        if pid <= 0 {
            continue;
        }
        if let Ok(info) = mac_identity(pid) {
            snapshot.insert(
                pid,
                MacProcess {
                    parent: info.pbi_ppid as i32,
                    group: info.pbi_pgid as i32,
                    start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                    alive: info.pbi_status != libc::SZOMB,
                },
            );
        }
    }
    Ok(snapshot)
}

#[cfg(target_os = "macos")]
fn mac_alive(pid: i32, identity: (u64, u64)) -> bool {
    mac_identity(pid).is_ok_and(|info| {
        (info.pbi_start_tvsec, info.pbi_start_tvusec) == identity && info.pbi_status != libc::SZOMB
    })
}

#[cfg(target_os = "macos")]
fn mac_signal(pid: i32, identity: (u64, u64), signal: i32) -> anyhow::Result<()> {
    let info = match mac_identity(pid) {
        Ok(info) => info,
        Err(error) => {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.raw_os_error() == Some(libc::ESRCH))
            {
                return Ok(());
            }
            return Err(error.context("cannot attest macOS process before signalling"));
        }
    };
    if (info.pbi_start_tvsec, info.pbi_start_tvusec) != identity || info.pbi_status == libc::SZOMB {
        return Ok(()); // Never signal a recycled PID.
    }
    ensure!(
        info.pbi_uid == unsafe { libc::geteuid() },
        "owned process UID changed"
    );
    // libproc has no pidfd: a same-UID exit/reuse between this check and kill
    // remains a platform race. Individual birth-checked signals avoid blindly
    // killing recycled PGIDs; they are not kernel-pinned process authority.
    if unsafe { libc::kill(pid, signal) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

pub(super) struct OwnedTree {
    root: i32,
    /// Set only after successful final cleanup. Partial cleanup remains retryable.
    cleaned: bool,
    /// The owner adopts request orphans; foreign-child test trees opt in explicitly.
    #[cfg(target_os = "linux")]
    adopt: Option<i32>,
    excluded: i32,
    #[cfg(target_os = "linux")]
    processes: BTreeMap<i32, OwnedFd>,
    #[cfg(target_os = "macos")]
    identity: (u64, u64),
    #[cfg(target_os = "macos")]
    parent: i32,
    #[cfg(target_os = "macos")]
    processes: BTreeMap<i32, (u64, u64)>,
}
impl OwnedTree {
    pub(super) fn new(root: i32, parent: i32, _adopt: bool, excluded: i32) -> anyhow::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let before = stat(root)?;
            ensure!(
                (root == std::process::id() as i32 || before.0 == parent) && before.1 == root,
                "worker is not an owned frontend process group"
            );
            let fd = pidfd(root)?;
            ensure!(
                stat(root)?.2 == before.2,
                "worker identity changed during registration"
            );
            Ok(Self {
                root,
                cleaned: false,
                adopt: _adopt.then_some(parent),
                excluded,
                processes: BTreeMap::from([(root, fd)]),
            })
        }
        #[cfg(target_os = "macos")]
        {
            let info = mac_identity(root)?;
            ensure!(
                (root == std::process::id() as i32 || info.pbi_ppid == parent as u32)
                    && info.pbi_pgid == root as u32,
                "worker is not an owned frontend process group"
            );
            Ok(Self {
                root,
                excluded,
                identity: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                cleaned: false,
                parent,
                processes: BTreeMap::from([(root, (info.pbi_start_tvsec, info.pbi_start_tvusec))]),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("request process ownership is unsupported on this platform")
    }
    pub(super) fn owner(parent: i32) -> anyhow::Result<Self> {
        #[allow(unused_mut)]
        let mut tree = Self::new(std::process::id() as i32, parent, false, 0)?;
        #[cfg(target_os = "linux")]
        {
            tree.adopt = Some(std::process::id() as i32);
        }
        Ok(tree)
    }

    pub(super) fn refresh(&mut self) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let mut dead = Vec::new();
            for (&pid, fd) in &self.processes {
                if pid != self.root && exited(fd) {
                    dead.push(pid);
                }
            }
            for pid in dead {
                if self.adopt.is_some() && self.root != std::process::id() as i32 {
                    unsafe {
                        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                    }
                }
                self.processes.remove(&pid);
            }
            let mut snapshot = BTreeMap::new();
            for entry in std::fs::read_dir("/proc")? {
                let entry = entry?;
                if let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<i32>().ok())
                    && let Ok(info) = stat(pid)
                {
                    snapshot.insert(pid, info);
                }
            }
            // Dead pidfds remain pinned but their numeric PIDs must never be
            // treated as ancestry roots after PID reuse.
            let mut owned: BTreeSet<i32> = self
                .processes
                .iter()
                .filter(|(_, fd)| !exited(fd))
                .map(|(&pid, _)| pid)
                .collect();
            loop {
                let before = owned.len();
                for (&pid, &(parent, _, _)) in &snapshot {
                    if pid != self.excluded
                        && (owned.contains(&parent) || self.adopt == Some(parent))
                    {
                        owned.insert(pid);
                    }
                }
                if before == owned.len() {
                    break;
                }
            }
            for pid in owned {
                if self.processes.contains_key(&pid) {
                    continue;
                }
                if let (Some(before), Ok(fd)) = (snapshot.get(&pid), pidfd(pid))
                    && stat(pid).is_ok_and(|after| after.2 == before.2)
                {
                    self.processes.insert(pid, fd);
                }
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let snapshot = mac_snapshot()?;
            if let Some(info) = snapshot.get(&self.root) {
                ensure!(
                    info.start == self.identity,
                    "owned worker identity changed; refusing numeric-PID signalling"
                );
            }
            discover_mac_descendants(&mut self.processes, &snapshot, self.parent, self.excluded);
            ensure!(
                self.processes.len() <= 65_536,
                "macOS request process identity budget exceeded"
            );
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("request process ownership is unsupported")
    }
    pub(super) fn signal_root(&self, sig: i32) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        return signal(&self.processes[&self.root], sig);
        #[cfg(target_os = "macos")]
        {
            let info = mac_identity(self.root)?;
            ensure!(
                (info.pbi_start_tvsec, info.pbi_start_tvusec) == self.identity,
                "owned worker identity changed; refusing numeric-PID signalling"
            );
            mac_signal(self.root, self.identity, sig)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("request process ownership is unsupported")
    }
    pub(super) fn root_exited(&self) -> bool {
        #[cfg(target_os = "linux")]
        return exited(&self.processes[&self.root]);
        #[cfg(target_os = "macos")]
        return mac_identity(self.root).map_or(true, |info| {
            (info.pbi_start_tvsec, info.pbi_start_tvusec) != self.identity
                || info.pbi_status == libc::SZOMB
        });
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        true
    }
    /// Wait on the pinned Linux process, without adding a polling interval to
    /// an already completed exit. The final cleanup still discovers orphans.
    pub(super) fn wait_root(&self, timeout: Duration) -> anyhow::Result<bool> {
        let deadline = Instant::now() + timeout;
        while !self.root_exited() && Instant::now() < deadline {
            #[cfg(target_os = "linux")]
            wait_pidfds(
                vec![libc::pollfd {
                    fd: self.processes[&self.root].as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }],
                deadline,
            )?;
            #[cfg(not(target_os = "linux"))]
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(self.root_exited())
    }
    /// Stop every forker before killing: repeated discovery closes the snapshot
    /// race for children in different groups/sessions and double-fork orphans.
    pub(super) fn cleanup(&mut self, kill_root: bool) -> anyhow::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            let result = (|| -> anyhow::Result<()> {
                loop {
                    self.refresh()?;
                    for (&pid, fd) in &self.processes {
                        if pid == self.root && self.root == std::process::id() as i32 {
                            continue;
                        }
                        if !exited(fd) {
                            signal(fd, libc::SIGSTOP)?;
                        }
                    }
                    // SIGSTOP delivery is asynchronous. Observe every live
                    // forker stopped before discovering children again.
                    loop {
                        let mut quiescent = true;
                        for (&pid, fd) in &self.processes {
                            if pid == self.root && self.root == std::process::id() as i32 {
                                continue;
                            }
                            quiescent &= process_stopped(pid, fd)?;
                        }
                        if quiescent {
                            break;
                        }
                        ensure!(
                            Instant::now() < deadline,
                            "request tree would not stop for cleanup"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    self.refresh()?;
                    let mut quiescent = true;
                    for (&pid, fd) in &self.processes {
                        if pid == self.root && self.root == std::process::id() as i32 {
                            continue;
                        }
                        quiescent &= process_stopped(pid, fd)?;
                    }
                    if quiescent {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "request tree would not quiesce for cleanup"
                    );
                }
                for (&pid, fd) in &self.processes {
                    if pid != self.root {
                        signal(fd, libc::SIGKILL)?;
                    }
                }
                if kill_root {
                    self.signal_root(libc::SIGKILL)?;
                } else {
                    self.signal_root(libc::SIGCONT)?;
                }
                while self
                    .processes
                    .iter()
                    .any(|(&pid, fd)| (pid != self.root || kill_root) && !exited(fd))
                {
                    ensure!(
                        Instant::now() < deadline,
                        "request descendant cleanup timed out"
                    );
                    let fds = self
                        .processes
                        .iter()
                        .filter(|(pid, fd)| (**pid != self.root || kill_root) && !exited(fd))
                        .map(|(_, fd)| libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        })
                        .collect();
                    wait_pidfds(fds, deadline)?;
                }
                // Only reap adopted descendants of this frontend, never
                // another request or the service. Child owns the root wait.
                if self.adopt.is_some() {
                    for &pid in self.processes.keys() {
                        if pid != self.root {
                            unsafe {
                                libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                            }
                        }
                    }
                }
                if self.root == std::process::id() as i32 {
                    // Managed Child waiters have finished before normal owner cleanup.
                    while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
                }
                Ok(())
            })();
            if result.is_err() {
                // Do not leave a still-owned process suspended on a failed
                // stop/discovery/kill path; Drop can retry final cleanup.
                for fd in self.processes.values() {
                    let _ = signal(fd, libc::SIGCONT);
                }
            } else if kill_root || self.root == std::process::id() as i32 {
                self.cleaned = true;
            }
            result
        }
        #[cfg(target_os = "macos")]
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stopped = BTreeSet::new();
            // Stop the root BEFORE enumerating/killing children. This prevents
            // normal executor workload groups escaping merely because the
            // worker died first. Each discovered forker is stopped in turn.
            if self.root != std::process::id() as i32 {
                mac_signal(self.root, self.identity, libc::SIGSTOP)?;
            }
            let result = (|| -> anyhow::Result<()> {
                loop {
                    self.refresh()?;
                    for (&pid, &identity) in &self.processes {
                        if pid == self.root && self.root == std::process::id() as i32 {
                            continue;
                        }
                        if stopped.insert((pid, identity)) {
                            mac_signal(pid, identity, libc::SIGSTOP)?;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    let before = self.processes.clone();
                    self.refresh()?;
                    if before == self.processes {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "macOS request descendants would not quiesce"
                    );
                }
                for (&pid, &identity) in &self.processes {
                    if pid != self.root {
                        mac_signal(pid, identity, libc::SIGKILL)?;
                    }
                }
                if kill_root {
                    mac_signal(self.root, self.identity, libc::SIGKILL)?;
                }
                while self.processes.iter().any(|(&pid, &identity)| {
                    (pid != self.root || kill_root) && mac_alive(pid, identity)
                }) {
                    ensure!(
                        Instant::now() < deadline,
                        "macOS request descendant cleanup timed out"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            })();
            // Never leave a still-owned forker stopped on an error path. All
            // numeric signals recheck birth identity; never broadcast to a
            // possibly recycled process-group number.
            for (pid, identity) in stopped {
                if result.is_err() || pid == self.root && !kill_root {
                    let _ = mac_signal(pid, identity, libc::SIGCONT);
                }
            }
            if !kill_root || result.is_err() {
                let _ = mac_signal(self.root, self.identity, libc::SIGCONT);
            }
            if (kill_root || self.root == std::process::id() as i32) && result.is_ok() {
                self.cleaned = true;
            }
            result
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("request process cleanup is unsupported")
    }
}
impl Drop for OwnedTree {
    fn drop(&mut self) {
        let _ = self.cleanup(self.root != std::process::id() as i32);
    }
}

pub(super) struct TerminalOwner {
    fd: i32,
    original: i32,
}
impl TerminalOwner {
    pub(super) fn check_interactive() -> anyhow::Result<Option<i32>> {
        let original = unsafe { libc::tcgetpgrp(0) };
        if original < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTTY) {
                return Ok(None);
            }
            return Err(error.into());
        }
        ensure!(
            original == unsafe { libc::getpgrp() },
            "interactive Job requires the frontend foreground process group; bring it to the foreground first"
        );
        Ok(Some(original))
    }
    pub(super) fn give_to(pid: i32) -> anyhow::Result<Option<Self>> {
        let Some(original) = Self::check_interactive()? else {
            return Ok(None);
        };
        set_foreground(0, pid)?;
        unsafe {
            libc::kill(-pid, libc::SIGCONT);
        }
        Ok(Some(Self { fd: 0, original }))
    }
}
impl Drop for TerminalOwner {
    fn drop(&mut self) {
        let _ = set_foreground(self.fd, self.original);
    }
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn owner_observation_preserves_managed_child_wait_and_finalizes_only_at_cleanup() {
        const NAME: &str =
            "owner_observation_preserves_managed_child_wait_and_finalizes_only_at_cleanup";
        if std::env::var("PVISOR_TREE_TEST_CHILD").as_deref() != Ok(NAME) {
            let module = module_path!().split_once("::").unwrap().1;
            assert!(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &format!("{module}::{NAME}"), "--test-threads=1"])
                    .env("PVISOR_TREE_TEST_CHILD", NAME)
                    .process_group(0)
                    .status()
                    .unwrap()
                    .success()
            );
            return;
        }
        become_subreaper().unwrap();
        let mut owner = OwnedTree::owner(unsafe { libc::getppid() }).unwrap();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let process = pidfd(child.id() as i32).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !exited(&process) {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        owner.refresh().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(23));
        assert!(!owner.cleaned);
        owner.cleanup(false).unwrap();
        assert!(owner.cleaned);
    }

    #[test]
    fn mac_discovery_owns_normal_workload_groups_without_recycled_ancestry() {
        use super::*;
        let process = |parent, group, start| MacProcess {
            parent,
            group,
            start: (start, 0),
            alive: true,
        };
        let mut owned = BTreeMap::from([(100, (1, 0))]);
        let mut snapshot = BTreeMap::from([
            (100, process(50, 100, 1)),
            (200, process(100, 200, 2)), // ProcessExecutor process_group(0)
            (201, process(200, 200, 3)),
            (300, process(200, 300, 4)), // separately grouped helper
            (50, process(1, 50, 5)),
            (60, process(50, 60, 6)), // independent persistent service
        ]);
        discover_mac_descendants(&mut owned, &snapshot, 50, 60);
        assert_eq!(
            owned.keys().copied().collect::<Vec<_>>(),
            [100, 200, 201, 300]
        );
        snapshot.remove(&200); // leader reaped, still-owned member reserves group
        snapshot.insert(202, process(1, 200, 7));
        discover_mac_descendants(&mut owned, &snapshot, 50, 60);
        assert!(owned.contains_key(&202));
        snapshot.insert(300, process(1, 300, 8)); // PID reused, unrelated process
        snapshot.insert(301, process(300, 301, 9));
        snapshot.insert(302, process(1, 300, 10));
        discover_mac_descendants(&mut owned, &snapshot, 50, 60);
        assert_eq!(owned[&300], (4, 0));
        assert!(!owned.contains_key(&301));
        assert!(!owned.contains_key(&302));
    }
    use super::*;
    use std::{os::unix::process::CommandExt, process::Command};

    #[test]
    fn cleanup_owns_double_forks_across_sessions_and_ignored_signals() {
        const NAME: &str = "cleanup_owns_double_forks_across_sessions_and_ignored_signals";
        if std::env::var("PVISOR_TREE_TEST_CHILD").as_deref() != Ok(NAME) {
            let module = module_path!().split_once("::").unwrap().1;
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("{module}::{NAME}"), "--test-threads=1"])
                .env("PVISOR_TREE_TEST_CHILD", NAME)
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        become_subreaper().unwrap();
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("escaped.pid");
        let mut child = Command::new("python3")
            .args([
                "-c",
                r#"
import os, signal, sys, time
signal.signal(signal.SIGINT, signal.SIG_IGN)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if os.fork() == 0:
    os.setsid()
    if os.fork() != 0: os._exit(0)
    with open(sys.argv[1], 'w') as f: f.write(str(os.getpid()))
while True: time.sleep(1)
"#,
            ])
            .arg(&marker)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut tree =
            OwnedTree::new(child.id() as i32, std::process::id() as i32, true, 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let escaped: i32 = loop {
            if let Some(pid) = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|s| s.parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "descendant readiness timed out");
            std::thread::sleep(Duration::from_millis(10));
        };
        let descendant = pidfd(escaped).unwrap();
        tree.refresh().unwrap();
        tree.signal_root(libc::SIGINT).unwrap();
        let started = Instant::now();
        tree.cleanup(true).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(exited(&descendant), "escaped session survived cleanup");
        assert!(!child.wait().unwrap().success());
        assert!(tree.cleaned);
        // Repeated finish/Drop must not adopt and kill a later independent
        // child merely because it now has the same frontend parent.
        let mut unrelated = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        tree.cleanup(false).unwrap();
        tree.cleanup(true).unwrap();
        let survived = unrelated.try_wait().unwrap().is_none();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        assert!(survived, "completed tree adopted a later independent child");
    }

    #[test]
    fn descendant_cleanup_does_not_finalize_live_root_ownership() {
        let mut child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut tree =
            OwnedTree::new(child.id() as i32, std::process::id() as i32, false, 0).unwrap();
        tree.cleanup(false).unwrap();
        assert!(!tree.cleaned);
        assert!(!tree.root_exited());
        tree.cleanup(true).unwrap();
        assert!(tree.cleaned);
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn root_exit_wait_preserves_child_reaping_ownership() {
        let mut child = Command::new("/bin/true").process_group(0).spawn().unwrap();
        let mut tree =
            OwnedTree::new(child.id() as i32, std::process::id() as i32, false, 0).unwrap();
        assert!(tree.wait_root(Duration::from_secs(2)).unwrap());
        tree.cleanup(true).unwrap();
        assert!(child.wait().unwrap().success());
        tree.cleanup(true).unwrap();
        assert!(tree.wait_root(Duration::ZERO).unwrap());
    }
}

fn set_foreground(fd: i32, pid: i32) -> anyhow::Result<()> {
    unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        let result = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        ensure!(result == 0, "block SIGTTOU: {result}");
        let result = libc::tcsetpgrp(fd, pid);
        let error = std::io::Error::last_os_error();
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if result < 0 {
            return Err(error.into());
        }
    }
    Ok(())
}

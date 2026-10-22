//! One process launcher and PID-1 reaper for every guest workload and exec.
use super::{check, resource};
use pvisor_guest::{GuestConfig, StdinMode};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
};

#[derive(Default)]
struct State {
    children: BTreeMap<u32, (mpsc::Sender<i32>, bool)>,
    stopping: bool,
}

#[derive(Clone, Default)]
pub(super) struct Processes(Arc<Mutex<State>>);

pub(super) struct Process {
    pid: u32,
    exit: mpsc::Receiver<i32>,
}
impl Process {
    pub(super) fn id(&self) -> u32 {
        self.pid
    }
    pub(super) fn wait(self) -> io::Result<i32> {
        self.exit
            .recv()
            .map_err(|_| io::Error::other("guest reaper stopped"))
    }
}

fn exited(options: i32) -> io::Result<Option<i32>> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | options,
            )
        } == 0
        {
            let pid = unsafe { info.si_pid() };
            return Ok((pid != 0).then_some(pid));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return if error.raw_os_error() == Some(libc::ECHILD) {
            Ok(None)
        } else {
            Err(error)
        };
    }
}

impl Processes {
    fn register(&self, child: std::process::Child, state: &mut State, group: bool) -> Process {
        let pid = child.id();
        let (sender, exit) = mpsc::channel();
        state.children.insert(pid, (sender, group));
        // Only supervise() consumes wait statuses; dropping Child never waits.
        Process { pid, exit }
    }
    pub(super) fn signal(&self, pid: u32, signal: i32) -> io::Result<()> {
        if !(1..=64).contains(&signal) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid signal",
            ));
        }
        let state = self.0.lock().unwrap();
        let (_, group) = state
            .children
            .get(&pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "guest process exited"))?;
        check(unsafe { libc::kill(if *group { -(pid as i32) } else { pid as i32 }, signal) })
    }
    pub(super) fn supervise(&self, main: Process) -> io::Result<i32> {
        let mut main_exit = None;
        loop {
            // Observe before consuming: the registry lock fences signals and
            // registrations against reaping and PID reuse, without polling.
            if exited(0)?.is_none() {
                return main_exit.ok_or_else(|| io::Error::other("workload exit was not observed"));
            }
            let mut state = self.0.lock().unwrap();
            // Command::spawn reaps failed execs itself while holding this lock.
            // Recheck the current event after admission, never consume a stale PID.
            let Some(pid) = exited(libc::WNOHANG)? else {
                continue;
            };
            if state
                .children
                .get(&(pid as u32))
                .is_some_and(|(_, group)| *group)
            {
                // The leader is still a zombie, so its group ID cannot be reused.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let mut status = 0;
            check(unsafe { libc::waitpid(pid, &mut status, 0) })?;
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            };
            if let Some((sender, _)) = state.children.remove(&(pid as u32)) {
                let _ = sender.send(code);
            }
            if pid as u32 == main.pid {
                main_exit = Some(code);
                state.stopping = true;
                // PID 1 owns all processes in this VM. Never use kill(-1) when
                // this supervisor is exercised by an unprivileged unit test.
                if unsafe { libc::getpid() } == 1 {
                    unsafe {
                        libc::kill(-1, libc::SIGKILL);
                    }
                } else {
                    for (pid, (_, group)) in &state.children {
                        unsafe {
                            libc::kill(
                                if *group { -(*pid as i32) } else { *pid as i32 },
                                libc::SIGKILL,
                            );
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn spawn_error(error: &io::Error) -> Option<i32> {
    match error.raw_os_error() {
        Some(libc::ENOENT) => Some(127),
        Some(libc::EACCES | libc::ENOEXEC) => Some(126),
        _ => None,
    }
}

pub(super) fn read_chunk(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    match reader.read(buffer) {
        Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
        result => result,
    }
}

fn identity(user: &str) -> io::Result<(u32, u32, Vec<u32>)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "unknown guest user/group");
    let (name, group) = user
        .split_once(':')
        .map_or((user, None), |(u, g)| (u, Some(g)));
    let name = if name.is_empty() { "0" } else { name };
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let entry = passwd
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|p| p.len() >= 4 && (p[0] == name || name.parse::<u32>().ok() == p[2].parse().ok()));
    let uid = name
        .parse()
        .ok()
        .or_else(|| entry.as_ref()?.get(2)?.parse().ok())
        .ok_or_else(invalid)?;
    let mut gid = entry.as_ref().and_then(|p| p[3].parse().ok()).unwrap_or(0);
    let groups = fs::read_to_string("/etc/group").unwrap_or_default();
    let mut additional = Vec::new();
    if let Some(group) = group {
        gid = group
            .parse()
            .ok()
            .or_else(|| {
                groups.lines().find_map(|l| {
                    let p: Vec<_> = l.split(':').collect();
                    (p.len() >= 3 && p[0] == group)
                        .then(|| p[2].parse().ok())
                        .flatten()
                })
            })
            .ok_or_else(invalid)?;
    } else if let Some(entry) = entry {
        for line in groups.lines() {
            let p: Vec<_> = line.split(':').collect();
            if p.len() >= 4
                && p[3].split(',').any(|member| member == entry[0])
                && let Ok(group) = p[2].parse()
            {
                additional.push(group);
            }
        }
    }
    Ok((uid, gid, additional))
}

fn command(config: &GuestConfig) -> io::Result<Command> {
    let mut command = config.command()?;
    let options = config.process.clone().unwrap_or_default();
    let identity = options.user.as_deref().map(identity).transpose()?;
    if let Some((uid, _, _)) = &identity
        && !config.env.contains_key("HOME")
    {
        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        let home = passwd
            .lines()
            .find_map(|line| {
                let fields: Vec<_> = line.split(':').collect();
                (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(*uid))
                    .then(|| fields[5])
            })
            .unwrap_or("/");
        command.env("HOME", home);
    }
    let limits = config
        .limits
        .iter()
        .map(|(name, (soft, hard))| {
            Ok((
                resource(name)?,
                libc::rlimit {
                    rlim_cur: *soft,
                    rlim_max: *hard,
                },
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    unsafe {
        command.pre_exec(move || {
            if options.new_session {
                check(libc::setsid())?;
            }
            for (resource, limit) in &limits {
                check(libc::syscall(
                    libc::SYS_prlimit64,
                    0,
                    *resource,
                    limit,
                    std::ptr::null::<libc::rlimit>(),
                ) as i32)?;
            }
            if let Some(capabilities) = &options.capabilities {
                for capability in 0..64 {
                    if !capabilities.contains(&capability) {
                        let result = libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0);
                        if result < 0
                            && io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
                        {
                            check(result)?;
                        }
                    }
                }
            }
            if let Some((uid, gid, groups)) = &identity {
                check(libc::setgroups(groups.len(), groups.as_ptr()))?;
                check(libc::setgid(*gid))?;
                check(libc::setuid(*uid))?;
            }
            Ok(())
        });
    }
    Ok(command)
}

pub(super) type ProcessOutput = Vec<(Box<dyn Read + Send>, u8)>;
pub(super) struct Spawned {
    pub(super) process: Process,
    pub(super) input: Option<Box<dyn Write + Send>>,
    pub(super) outputs: ProcessOutput,
}

impl Processes {
    pub(super) fn spawn(&self, config: &GuestConfig, capture: bool) -> io::Result<Spawned> {
        let mut command = command(config)?;
        let options = config.process.clone().unwrap_or_default();
        if options.create_workdir {
            fs::create_dir_all(&config.cwd)?;
        }
        let mut state = self.0.lock().unwrap();
        if state.stopping {
            return Err(io::Error::other("guest is stopping"));
        }
        let interactive = matches!(options.stdin, StdinMode::Pipe);
        if let Some([rows, columns]) = options.tty {
            let mut master = -1;
            let mut slave = -1;
            let size = libc::winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            check(unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            })?;
            let master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            let fd = slave.as_raw_fd();
            unsafe { command.pre_exec(move || check(libc::ioctl(fd, libc::TIOCSCTTY, 0))) };
            command
                .stdin(slave.try_clone()?)
                .stdout(slave.try_clone()?)
                .stderr(slave);
            let child = command.spawn()?;
            let input = if interactive {
                Some(Box::new(master.try_clone()?) as Box<dyn Write + Send>)
            } else {
                None
            };
            Ok(Spawned {
                process: self.register(child, &mut state, options.new_session),
                input,
                outputs: vec![(Box::new(master), 1)],
            })
        } else {
            command.stdin(match options.stdin {
                StdinMode::Inherit => Stdio::inherit(),
                StdinMode::Closed => Stdio::null(),
                StdinMode::Pipe => Stdio::piped(),
            });
            if capture {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            let mut child = command.spawn()?;
            let input = child
                .stdin
                .take()
                .map(|s| Box::new(s) as Box<dyn Write + Send>);
            let output = if capture {
                vec![
                    (
                        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
                        1,
                    ),
                    (
                        Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
                        2,
                    ),
                ]
            } else {
                Vec::new()
            };
            Ok(Spawned {
                process: self.register(child, &mut state, options.new_session),
                input,
                outputs: output,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_reaper_routes_exec_exits_and_reaps_orphans_while_main_is_alive() {
        // nextest runs this in its own process; adopted test children stay here.
        check(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let done = temporary.path().join("exec-done");
        let processes = Processes::default();
        let config = GuestConfig {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                include_str!("../../tests/fixtures/reap_orphans.sh").into(),
            ],
            env: BTreeMap::from([("WAIT_FOR".into(), done.to_str().unwrap().into())]),
            cwd: "/".into(),
            ..Default::default()
        };
        let main = processes.spawn(&config, false).unwrap().process;
        let control = processes.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..32 {
                let config = GuestConfig {
                    argv: vec!["/pvisor-no-such-command".into()],
                    cwd: "/".into(),
                    ..Default::default()
                };
                assert_eq!(
                    control.spawn(&config, false).err().unwrap().raw_os_error(),
                    Some(libc::ENOENT)
                );
            }
            for code in [7, 23, 0, 19] {
                let config = GuestConfig {
                    argv: vec!["/bin/sh".into(), "-c".into(), format!("exit {code}")],
                    cwd: "/".into(),
                    process: Some(pvisor_guest::ProcessConfig {
                        new_session: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let process = control.spawn(&config, false).unwrap().process;
                let pid = process.id();
                assert_eq!(process.wait().unwrap(), code);
                assert_eq!(
                    control.signal(pid, 9).unwrap_err().kind(),
                    io::ErrorKind::NotFound
                );
            }
            let config = GuestConfig {
                argv: vec!["/bin/sleep".into(), "10".into()],
                cwd: "/".into(),
                ..Default::default()
            };
            let process = control.spawn(&config, false).unwrap().process;
            assert!(control.signal(process.id(), 0).is_err());
            control.signal(process.id(), libc::SIGTERM).unwrap();
            assert_eq!(process.wait().unwrap(), 143);
            fs::write(done, b"done").unwrap();
        });
        assert_eq!(processes.supervise(main).unwrap(), 0);
        worker.join().unwrap();
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) }, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

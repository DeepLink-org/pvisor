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
    children: BTreeMap<u32, (mpsc::Sender<i32>, bool, Option<File>, u64)>,
    next_generation: u64,
    stopping: bool,
}

#[derive(Clone, Default)]
pub(super) struct Processes(Arc<Mutex<State>>);

pub(super) struct Process {
    pid: u32,
    generation: u64,
    exit: mpsc::Receiver<i32>,
}
impl Process {
    pub(super) fn id(&self) -> u32 {
        self.pid
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
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
    fn register(
        &self,
        child: std::process::Child,
        state: &mut State,
        group: bool,
        terminal: Option<File>,
    ) -> Process {
        let pid = child.id();
        let (sender, exit) = mpsc::channel();
        state.next_generation += 1;
        let generation = state.next_generation;
        state
            .children
            .insert(pid, (sender, group, terminal, generation));
        // Only supervise() consumes wait statuses; dropping Child never waits.
        Process {
            pid,
            generation,
            exit,
        }
    }
    pub(super) fn signal(&self, pid: u32, signal: i32) -> io::Result<()> {
        self.signal_exact(pid, None, signal)
    }
    pub(super) fn signal_tracked(&self, pid: u32, generation: u64, signal: i32) -> io::Result<()> {
        self.signal_exact(pid, Some(generation), signal)
    }
    fn signal_exact(&self, pid: u32, generation: Option<u64>, signal: i32) -> io::Result<()> {
        if !(1..=64).contains(&signal) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid signal",
            ));
        }
        let state = self.0.lock().unwrap();
        let (_, group, _, actual_generation) = state
            .children
            .get(&pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "guest process exited"))?;
        if generation.is_some_and(|value| value != *actual_generation) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "guest process generation expired",
            ));
        }
        check(unsafe { libc::kill(if *group { -(pid as i32) } else { pid as i32 }, signal) })
    }
    pub(super) fn resize(
        &self,
        pid: u32,
        generation: u64,
        rows: u16,
        columns: u16,
    ) -> io::Result<()> {
        if rows == 0 || columns == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY dimensions must be nonzero",
            ));
        }
        let state = self.0.lock().unwrap();
        let (_, _, terminal, actual_generation) = state
            .children
            .get(&pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "guest process exited"))?;
        if generation != *actual_generation {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "guest process generation expired",
            ));
        }
        let terminal = terminal
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "process has no PTY"))?;
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        check(unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ, &size) })
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
                .is_some_and(|(_, group, _, _)| *group)
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
            if let Some((sender, _, _, _)) = state.children.remove(&(pid as u32)) {
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
                    for (pid, (_, group, _, _)) in &state.children {
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
    loop {
        match reader.read(buffer) {
            Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // Attached PTY input and output share a nonblocking master.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
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

/// The passwd home directory of `uid`, or the filesystem root when the
/// image ships no matching entry — the same defaulting semantics Docker
/// applies when the environment omits HOME. Malformed lines never match.
pub(crate) fn default_home(uid: u32, passwd: &str) -> &str {
    passwd
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            // `then`, not `then_some`: the index must only be evaluated for
            // well-formed lines.
            (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid)).then(|| fields[5])
        })
        .unwrap_or("/")
}

fn command(config: &GuestConfig) -> io::Result<Command> {
    let mut command = config.command()?;
    let options = config.process.clone().unwrap_or_default();
    let identity = options.user.as_deref().map(identity).transpose()?;
    if (config.process.is_none() || options.user.is_some()) && !config.env.contains_key("HOME") {
        // Only explicit identity selection requests passwd defaults. Native
        // stage launches preserve missing HOME as an environment tombstone.
        let uid = identity.as_ref().map(|(uid, _, _)| *uid).unwrap_or(0);
        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        command.env("HOME", default_home(uid, &passwd));
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

fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    check(flags)?;
    check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
    Ok(())
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
        let capture = capture || options.attached;
        let cancellable_input = options.attached && !config.control;
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
            for file in [&master, &slave] {
                check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
            }
            if cancellable_input && interactive {
                nonblocking(&master)?;
            }
            let fd = slave.as_raw_fd();
            unsafe { command.pre_exec(move || check(libc::ioctl(fd, libc::TIOCSCTTY, 0))) };
            command
                .stdin(slave.try_clone()?)
                .stdout(slave.try_clone()?)
                .stderr(slave);
            if options.attached && matches!(options.stdin, StdinMode::Closed) {
                command.stdin(Stdio::null());
            }
            // Acquire every fallible descriptor before spawning: a successful
            // child must always enter the PID-1 registry, including EMFILE paths.
            let terminal = master.try_clone()?;
            let input = if interactive {
                Some(Box::new(master.try_clone()?) as Box<dyn Write + Send>)
            } else {
                None
            };
            let child = command.spawn()?;
            Ok(Spawned {
                process: self.register(child, &mut state, options.new_session, Some(terminal)),
                input,
                outputs: vec![(Box::new(master), 1)],
            })
        } else {
            command.stdin(match options.stdin {
                StdinMode::Inherit => Stdio::inherit(),
                StdinMode::Closed => Stdio::null(),
                StdinMode::Pipe => Stdio::piped(),
            });
            let attached_input = if cancellable_input && interactive {
                let mut fds = [-1; 2];
                check(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) })?;
                let reader = unsafe { File::from_raw_fd(fds[0]) };
                let writer = unsafe { File::from_raw_fd(fds[1]) };
                nonblocking(&writer)?;
                command.stdin(reader);
                Some(Box::new(writer) as Box<dyn Write + Send>)
            } else {
                None
            };
            if capture {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            let mut child = command.spawn()?;
            let input = attached_input.or_else(|| {
                child
                    .stdin
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Write + Send>)
            });
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
                process: self.register(child, &mut state, options.new_session, None),
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
    fn attached_launch_preserves_absent_home_but_legacy_launch_defaults_it() {
        let mut config = GuestConfig {
            argv: vec!["/bin/true".into()],
            cwd: "/".into(),
            process: Some(pvisor_guest::ProcessConfig {
                attached: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            !command(&config)
                .unwrap()
                .get_envs()
                .any(|(key, _)| key == "HOME")
        );
        config.process = None;
        assert!(
            command(&config)
                .unwrap()
                .get_envs()
                .any(|(key, _)| key == "HOME")
        );
    }

    fn attached_config(script: &str, tty: Option<[u16; 2]>, interactive: bool) -> GuestConfig {
        GuestConfig {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: "/".into(),
            process: Some(pvisor_guest::ProcessConfig {
                attached: true,
                new_session: true,
                stdin: if interactive {
                    StdinMode::Pipe
                } else {
                    StdinMode::Closed
                },
                tty,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn attached_pipes_keep_streams_separate_and_stdin_reaches_eof() {
        let processes = Processes::default();
        let config = attached_config(
            "test ! -t 0 && test ! -t 1 && test ! -t 2 || exit 1; cat; printf err >&2",
            None,
            true,
        );
        let Spawned {
            process,
            mut input,
            outputs,
        } = processes.spawn(&config, false).unwrap();
        input.as_mut().unwrap().write_all(b"input").unwrap();
        drop(input);
        assert_eq!(processes.supervise(process).unwrap(), 0);
        let mut data = BTreeMap::new();
        for (mut output, fd) in outputs {
            let mut bytes = Vec::new();
            output.read_to_end(&mut bytes).unwrap();
            data.insert(fd, bytes);
        }
        assert_eq!(data[&1], b"input");
        assert_eq!(data[&2], b"err");
    }

    #[test]
    fn attached_pty_has_dimensions_combined_output_and_closed_stdin_eof() {
        let processes = Processes::default();
        let config = attached_config(
            "test -t 1 && test -t 2 || exit 1; read line && exit 2; stty size <&1; printf err >&2",
            Some([31, 97]),
            false,
        );
        let Spawned {
            process,
            input,
            mut outputs,
        } = processes.spawn(&config, false).unwrap();
        assert!(input.is_none());
        assert_eq!(processes.supervise(process).unwrap(), 0);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].1, 1);
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = read_chunk(&mut outputs[0].0, &mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(bytes, b"31 97\r\nerr");
    }

    #[test]
    fn attached_interactive_pty_accepts_input() {
        let processes = Processes::default();
        let config = attached_config(
            "test -t 0 || exit 1; read line; test \"$line\" = hello",
            Some([24, 80]),
            true,
        );
        let Spawned {
            process,
            mut input,
            outputs: _outputs,
        } = processes.spawn(&config, false).unwrap();
        input.as_mut().unwrap().write_all(b"hello\n").unwrap();
        assert_eq!(processes.supervise(process).unwrap(), 0);
    }

    #[test]
    fn default_home_follows_the_effective_uid_and_falls_back_to_root_dir() {
        let passwd = concat!(
            "root:x:0:0:root:/root:/bin/sh\n",
            "www-data:x:33:33::/var/www:/usr/sbin/nologin\n",
            "malformed line without fields\n",
        );
        assert_eq!(default_home(0, passwd), "/root");
        assert_eq!(default_home(33, passwd), "/var/www");
        assert_eq!(default_home(1000, passwd), "/");
        assert_eq!(default_home(0, ""), "/");
    }

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

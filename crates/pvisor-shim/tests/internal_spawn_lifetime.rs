//! Regression coverage for the internal-spawn parent-death contract.
//!
//! Internal shim children that arm `PR_SET_PDEATHSIG` (the VM runner and the
//! sandbox holder) must be forked from the dedicated spawner thread. Forking
//! from a pooled worker thread instead ties their death signal to that
//! thread's idle exit: a healthy VM runner and its sandbox were observed
//! dying with exit status 143 exactly ~11 s after creation, when the
//! `spawn_blocking` thread that forked the runner hit its keep-alive.
//!
//! The control case reproduces the mechanism directly (a pdeath-armed child
//! forked on a short-lived pool thread dies as soon as the thread idles
//! out); the fix case asserts the same child forked through
//! [`pvisor_shim::child::fork_internal`] survives the pool thread's exit.
//!
//! Custom harness: the binary re-execs itself in child mode, which the
//! libtest harness would reject as unknown arguments.

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const CHILD_MARKER: &str = "pvisor-chaos-pdeath-child";
    const POOL_KEEP_ALIVE: Duration = Duration::from_millis(100);

    /// Child mode: arm the parent-death signal exactly like the shim's
    /// internal children, report readiness, then sleep. Killed by SIGTERM
    /// when the parent *thread* that forked us exits; exits 86 when the
    /// parent was already gone (the production fork/prctl race check).
    pub fn child_main(parent_pid: &str) {
        unsafe {
            assert_eq!(
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0),
                0,
                "arm PR_SET_PDEATHSIG"
            );
        }
        let expected: libc::pid_t = parent_pid.parse().expect("parent pid argument");
        if unsafe { libc::getppid() } != expected {
            std::process::exit(86);
        }
        println!("READY");
        std::thread::sleep(Duration::from_secs(30));
        std::process::exit(0);
    }

    fn child_command() -> Command {
        let exe = std::env::current_exe().expect("current test binary");
        let mut command = Command::new(exe);
        command
            .arg(CHILD_MARKER)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }

    /// Wait for the child's READY line so the assertion phase never races
    /// the child's own startup.
    fn await_ready(child: &mut Child) {
        let stdout = child.stdout.take().expect("piped child stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read child READY line");
        assert_eq!(line.trim(), "READY", "child did not arm cleanly: {line:?}");
    }

    fn exited_by_sigterm(child: &mut Child, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().expect("poll child") {
                return status.signal() == Some(libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// Fork the child on a blocking-pool thread of a runtime whose pool
    /// threads idle-exit after `POOL_KEEP_ALIVE`, keep the runtime alive
    /// past that deadline, then hand the child back with the runtime gone.
    fn fork_from_short_lived_pool(direct: bool) -> Child {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .thread_keep_alive(POOL_KEEP_ALIVE)
            .build()
            .expect("build short-lived-pool runtime");
        let mut child = runtime.block_on(async {
            tokio::task::spawn_blocking(move || {
                if direct {
                    child_command().spawn().expect("spawn on pool thread")
                } else {
                    pvisor_shim::child::fork_internal(child_command())
                        .expect("spawn via dedicated fork thread")
                }
            })
            .await
            .expect("blocking task join")
        });
        await_ready(&mut child);
        // Outlive the pool keep-alive: a pooled forking thread has exited
        // by the end of this sleep, while the shim process still runs.
        std::thread::sleep(POOL_KEEP_ALIVE * 6);
        drop(runtime);
        child
    }

    pub fn entry() {
        // Control: a pdeath child forked directly on a pooled thread dies
        // with SIGTERM once that thread idles out. This proves the harness
        // reproduces the original failure mechanism.
        let mut pooled_child = fork_from_short_lived_pool(true);
        assert!(
            exited_by_sigterm(&mut pooled_child, Duration::from_secs(3)),
            "control child on a pooled thread was not parent-death killed; \
             the reproduction mechanism itself failed"
        );

        // Fix: the same child forked via the dedicated spawner thread
        // survives the pool thread's exit (and the runtime's teardown).
        let mut dedicated_child = fork_from_short_lived_pool(false);
        std::thread::sleep(Duration::from_secs(2));
        assert!(
            dedicated_child
                .try_wait()
                .expect("poll dedicated child")
                .is_none(),
            "child forked via the dedicated thread died with the pool thread"
        );
        dedicated_child.kill().expect("kill dedicated child");
        let status = dedicated_child.wait().expect("reap dedicated child");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    if args.get(1).map(String::as_str) == Some("pvisor-chaos-pdeath-child") {
        linux::child_main(args.get(2).expect("parent pid argument"));
        return;
    }
    #[cfg(target_os = "linux")]
    linux::entry();
}

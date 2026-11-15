//! Parent-death signals follow the spawning thread.
//!
//! A pooled-thread child must receive SIGTERM on thread exit; a child spawned
//! through `fork_internal` must remain responsive. The owning libtest test
//! invokes the ignored probe through a private readiness socket.

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const PARENT_PID: &str = "PVISOR_SHIM_TEST_PARENT_PID";
    const POOL_KEEP_ALIVE: Duration = Duration::from_millis(100);
    const DEADLINE: Duration = Duration::from_secs(3);

    /// Arm pdeath while the spawning thread is still alive, then acknowledge
    /// pings. The socket is supplied as stdin, separate from libtest's output.
    #[test]
    #[ignore = "subprocess-only probe; invoked by internal_children_follow_the_forking_thread_lifetime"]
    fn parent_death_probe() {
        let expected: libc::pid_t = std::env::var(PARENT_PID)
            .expect("probe must be launched by its owning test")
            .parse()
            .unwrap();
        let mut channel = File::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
        unsafe {
            assert_eq!(
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0),
                0,
                "arm PR_SET_PDEATHSIG"
            );
        }
        if unsafe { libc::getppid() } != expected {
            std::process::exit(86);
        }
        channel.write_all(b"R").unwrap();
        let mut ping = [0];
        while channel.read_exact(&mut ping).is_ok() {
            channel.write_all(&ping).unwrap();
        }
    }

    fn child_command() -> (Command, UnixStream) {
        let exe = std::env::current_exe().expect("current test binary");
        let (channel, child_channel) = UnixStream::pair().unwrap();
        channel.set_read_timeout(Some(DEADLINE)).unwrap();
        channel.set_write_timeout(Some(DEADLINE)).unwrap();
        let mut command = Command::new(exe);
        command
            .args([
                "--exact",
                "linux::parent_death_probe",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PARENT_PID, std::process::id().to_string())
            .stdin(Stdio::from(OwnedFd::from(child_channel)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        (command, channel)
    }

    struct Probe {
        child: Child,
        channel: UnixStream,
    }

    impl Probe {
        fn expect_byte(&mut self, expected: u8) {
            let mut byte = [0];
            self.channel.read_exact(&mut byte).expect("probe response");
            assert_eq!(byte, [expected]);
        }

        fn exited_by_sigterm(&mut self) -> bool {
            let deadline = Instant::now() + DEADLINE;
            while Instant::now() < deadline {
                if let Some(status) = self.child.try_wait().expect("poll child") {
                    return status.signal() == Some(libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        }

        fn assert_running(&mut self) {
            self.channel.write_all(b"P").expect("ping surviving child");
            self.expect_byte(b'P');
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "child forked via the dedicated thread died with the pool thread"
            );
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            // Also reap on assertion failure; never leave an owned probe behind.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Fork the child on a blocking-pool thread of a runtime whose pool
    /// threads idle-exit after `POOL_KEEP_ALIVE`. Observe actual thread exit
    /// before tearing down the runtime, rather than guessing with a sleep.
    fn fork_from_short_lived_pool(direct: bool) -> Probe {
        let (stopped, receiver) = mpsc::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .thread_keep_alive(POOL_KEEP_ALIVE)
            .on_thread_stop(move || {
                let _ = stopped.send(());
            })
            .build()
            .expect("build short-lived-pool runtime");
        let probe = runtime.block_on(async {
            tokio::task::spawn_blocking(move || {
                let (mut command, channel) = child_command();
                let child = if direct {
                    command.spawn().expect("spawn on pool thread")
                } else {
                    pvisor_shim::child::fork_internal(command)
                        .expect("spawn via dedicated fork thread")
                };
                let mut probe = Probe { child, channel };
                // Hold the spawning pool thread alive until pdeath is armed.
                probe.expect_byte(b'R');
                probe
            })
            .await
            .expect("blocking task join")
        });
        receiver
            .recv_timeout(DEADLINE)
            .expect("blocking pool thread did not idle-exit");
        drop(runtime);
        probe
    }

    #[test]
    fn internal_children_follow_the_forking_thread_lifetime() {
        // Control: parent-thread exit must deliver the configured death signal.
        let mut pooled_child = fork_from_short_lived_pool(true);
        assert!(
            pooled_child.exited_by_sigterm(),
            "control child on a pooled thread was not parent-death killed; \
             the reproduction mechanism itself failed"
        );

        // Fix: the same child forked via the dedicated spawner thread
        // survives the pool thread's exit (and the runtime's teardown).
        let mut dedicated_child = fork_from_short_lived_pool(false);
        dedicated_child.assert_running();
        dedicated_child.child.kill().expect("kill dedicated child");
        let status = dedicated_child.child.wait().expect("reap dedicated child");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }
}

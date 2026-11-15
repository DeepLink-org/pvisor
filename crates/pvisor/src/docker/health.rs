//! Persistent health observations owned by one container generation.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// Fully resolved health command. Durations are milliseconds; no host shell is used.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Healthcheck {
    /// Guest executable and literal arguments, including an explicit guest shell
    /// for CMD-SHELL. Runs as the workload user with its cwd/environment.
    pub command: Vec<String>,
    /// Delay between completed checks; must be positive and at most one day.
    pub interval_ms: u64,
    /// Execution timeout; the tracked guest process group is killed on timeout.
    /// Positive, at most one day; transport cleanup has an additional bounded wait.
    pub timeout_ms: u64,
    /// Consecutive failures needed to become unhealthy; must be nonzero.
    pub retries: u32,
    /// Initial grace: checks started during it do not count as failures.
    /// A success ends grace even before the configured period elapses.
    pub start_period_ms: u64,
    /// Check interval during initial grace; positive and at most one day.
    pub start_interval_ms: u64,
}

impl Healthcheck {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.command.is_empty()
                && !self.command[0].is_empty()
                && self.command.iter().all(|word| !word.contains('\0'))
                && self.command.iter().map(String::len).sum::<usize>() <= 64 * 1024,
            "invalid healthcheck command"
        );
        ensure!(
            (1..=86_400_000).contains(&self.interval_ms)
                && (1..=86_400_000).contains(&self.timeout_ms)
                && (1..=86_400_000).contains(&self.start_interval_ms)
                && self.start_period_ms <= 86_400_000
                && self.retries > 0,
            "invalid healthcheck timing or retries"
        );
        Ok(())
    }
}

/// Health state of a currently running generation, independent of process liveness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// No successful check yet and failure threshold not reached.
    Starting,
    /// A successful check established health and the failure threshold has not
    /// subsequently been reached.
    Healthy,
    /// Consecutive counted failures reached the configured threshold.
    Unhealthy,
}

impl std::fmt::Display for State {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Starting => "starting",
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
        })
    }
}

/// Latest bounded, private health observation; output can contain workload secrets.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// Exact container launch generation; stale records never imply readiness.
    pub generation: String,
    /// Latest projected health state.
    pub state: State,
    /// Consecutive counted failures since the last success.
    pub failing_streak: u32,
    /// Last check's exit code; timeout/transport failures use 125.
    pub exit_code: Option<i32>,
    /// At most 4096 bytes of lossy UTF-8 check output/diagnostic text.
    pub output: String,
}

impl Store {
    /// Observe health for the current running Compose generation. None means
    /// stopped or no configured effective healthcheck, not healthy.
    /// Missing/stale observations report Starting; corrupt records remain errors.
    /// This does not execute a new check or change the workload.
    pub fn health(&self, selector: &str) -> anyhow::Result<Option<Status>> {
        let instance = self.resolve(selector)?;
        if instance.state != "running"
            || instance
                .compose
                .as_ref()
                .and_then(|member| member.healthcheck.as_ref())
                .is_none()
        {
            return Ok(None);
        }
        let path = self.directory(&instance.id).join("health.json");
        match File::open(path) {
            Ok(file) => {
                let status: Status = serde_json::from_reader(file.take(32 * 1024))?;
                if status.generation == instance.generation {
                    return Ok(Some(status));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Some(Status {
            generation: instance.generation,
            state: State::Starting,
            failing_streak: 0,
            exit_code: None,
            output: String::new(),
        }))
    }
}

pub(super) struct Monitor {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl Monitor {
    pub(super) fn start(
        store: Store,
        instance: Instance,
        config: Healthcheck,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::Builder::new()
            .name("container-health".into())
            .spawn(move || {
                let started = Instant::now();
                let mut status = Status {
                    generation: instance.generation.clone(),
                    state: State::Starting,
                    failing_streak: 0,
                    exit_code: None,
                    output: String::new(),
                };
                let mut next = started
                    + Duration::from_millis(if config.start_period_ms > 0 {
                        config.start_interval_ms
                    } else {
                        config.interval_ms
                    });
                while !cancelled.load(Ordering::Acquire) {
                    if Instant::now() < next {
                        std::thread::sleep(
                            next.saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(20)),
                        );
                        continue;
                    }
                    let check_started = started.elapsed();
                    let result = probe(&store, &instance, &config, &cancelled);
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let (code, output) = match result {
                        Ok(result) => result,
                        Err(error) if error.is::<CancellationFailure>() => return Err(error),
                        Err(error) => (125, format!("{error:#}")),
                    };
                    update(&mut status, &config, check_started, code, output);
                    let _guard = store.lock()?;
                    let current = store.resolve(&instance.id)?;
                    ensure!(
                        current.generation == instance.generation && current.state == "running",
                        "health owner lost its container generation"
                    );
                    crate::util::write_private_json(
                        &store.directory(&instance.id).join("health.json"),
                        &status,
                    )?;
                    let grace = status.state == State::Starting
                        && started.elapsed() < Duration::from_millis(config.start_period_ms);
                    next = Instant::now()
                        + Duration::from_millis(if grace {
                            config.start_interval_ms
                        } else {
                            config.interval_ms
                        });
                }
                Ok(())
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }

    pub(super) fn check(&mut self) -> anyhow::Result<()> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            self.worker
                .take()
                .unwrap()
                .join()
                .map_err(|_| anyhow::anyhow!("health monitor panicked"))??;
            bail!("health monitor stopped unexpectedly");
        }
        Ok(())
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn update(status: &mut Status, config: &Healthcheck, elapsed: Duration, code: i32, output: String) {
    status.exit_code = Some(code);
    let mut end = output.len().min(4096);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    status.output = output[..end].to_owned();
    if code == 0 {
        status.failing_streak = 0;
        status.state = State::Healthy;
    } else if !(status.state == State::Starting
        && elapsed < Duration::from_millis(config.start_period_ms))
    {
        status.failing_streak = status.failing_streak.saturating_add(1);
        if status.failing_streak >= config.retries {
            status.state = State::Unhealthy;
        }
    }
}

struct ExecGuard<'a> {
    instance: &'a Instance,
    child: Option<(u32, u64)>,
    cancellation_failed: Arc<AtomicBool>,
}

#[derive(Debug)]
struct CancellationFailure;
impl std::fmt::Display for CancellationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("healthcheck cancellation was not confirmed; terminating its VM owner")
    }
}
impl std::error::Error for CancellationFailure {}

impl Drop for ExecGuard<'_> {
    fn drop(&mut self) {
        if let Some((pid, generation)) = self.child {
            // Explicit EOF already closed stdin, so disconnection alone is not
            // cancellation. Kill through the guest registry, never a host PID.
            let cancelled = (|| -> anyhow::Result<()> {
                let mut stream = UnixStream::connect(socket(self.instance)?)?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                write_frame(
                    &mut stream,
                    &Request::Signal {
                        pid,
                        generation,
                        signal: libc::SIGKILL,
                    },
                )?;
                ensure!(
                    matches!(read_frame::<Reply>(&mut stream)?, Reply::Ready),
                    "guest did not acknowledge healthcheck cancellation"
                );
                Ok(())
            })();
            if cancelled.is_err() {
                self.cancellation_failed.store(true, Ordering::Release);
            }
        }
    }
}

fn probe(
    store: &Store,
    instance: &Instance,
    config: &Healthcheck,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<(i32, String)> {
    let mut stream = store.connect(instance)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    let interrupt = stream.try_clone()?;
    let timeout = Duration::from_millis(config.timeout_ms);
    let stop = stop.clone();
    let expired = Arc::new(AtomicBool::new(false));
    let timed_out = expired.clone();
    let (done, completion) = mpsc::channel::<()>();
    let deadline = Instant::now() + timeout;
    let cancellation_failed = Arc::new(AtomicBool::new(false));
    let watcher = std::thread::spawn(move || {
        loop {
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                timed_out.store(!stop.load(Ordering::Acquire), Ordering::Release);
                let _ = interrupt.shutdown(std::net::Shutdown::Both);
                break;
            }
            match completion.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
    let result = (|| {
        write_frame(
            &mut stream,
            &Request::Start {
                launch: control::ExecLaunch {
                    argv: config.command.clone(),
                    env: BTreeMap::new(),
                    replace_env: false,
                    limits: BTreeMap::new(),
                    cwd: None,
                    user: None,
                    tty: None,
                },
            },
        )?;
        let mut guard = ExecGuard {
            instance,
            child: None,
            cancellation_failed: cancellation_failed.clone(),
        };
        match read_frame::<Reply>(&mut stream)? {
            Reply::Started { pid, generation } => guard.child = Some((pid, generation)),
            Reply::Error { message } => {
                if let Reply::Exit { code } = read_frame::<Reply>(&mut stream)? {
                    return Ok((code, message));
                }
                bail!("{message}");
            }
            _ => bail!("invalid healthcheck start response"),
        }
        write_frame(&mut stream, &control::StdinFrame::Eof {})?;
        let mut output = Vec::new();
        loop {
            match read_frame::<Reply>(&mut stream)? {
                Reply::Output { bytes, .. } => {
                    output.extend_from_slice(&bytes[..bytes.len().min(4096 - output.len())]);
                }
                Reply::Exit { code } => {
                    guard.child = None;
                    return Ok((code, String::from_utf8_lossy(&output).into_owned()));
                }
                Reply::Error { message } => bail!("{message}"),
                _ => bail!("invalid healthcheck response"),
            }
        }
    })();
    let _ = done.send(());
    let _ = watcher.join();
    if cancellation_failed.load(Ordering::Acquire) {
        return Err(CancellationFailure.into());
    }
    ensure!(
        !expired.load(Ordering::Acquire),
        "healthcheck timed out after {} ms",
        config.timeout_ms
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_grace_threshold_recovery_and_output_are_bounded() {
        let config = Healthcheck {
            command: vec!["true".into()],
            interval_ms: 100,
            timeout_ms: 100,
            retries: 2,
            start_period_ms: 1000,
            start_interval_ms: 50,
        };
        let mut status = Status {
            generation: "one".into(),
            state: State::Starting,
            failing_streak: 0,
            exit_code: None,
            output: String::new(),
        };
        update(
            &mut status,
            &config,
            Duration::from_millis(100),
            1,
            "not ready".into(),
        );
        assert_eq!(status.failing_streak, 0);
        update(
            &mut status,
            &config,
            Duration::from_millis(999),
            125,
            "check started within grace and completed after it".into(),
        );
        assert_eq!(status.failing_streak, 0);
        update(
            &mut status,
            &config,
            Duration::from_millis(200),
            0,
            "ready".into(),
        );
        assert_eq!(status.state, State::Healthy);
        update(
            &mut status,
            &config,
            Duration::from_millis(300),
            1,
            "failed".into(),
        );
        assert_eq!(status.failing_streak, 1, "success ends the initial grace");
        update(
            &mut status,
            &config,
            Duration::from_millis(400),
            1,
            "x".repeat(9000),
        );
        assert_eq!(status.state, State::Unhealthy);
        assert_eq!(status.output.len(), 4096);
        update(
            &mut status,
            &config,
            Duration::from_millis(500),
            0,
            String::new(),
        );
        assert_eq!(status.state, State::Healthy);
    }
}

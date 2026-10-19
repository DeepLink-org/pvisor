//! The daemon owns this worker's readiness, lifetime and exit status.
use anyhow::{Context, bail};
use std::{io::Write, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, Command},
};

pub(super) struct RunningCache {
    child: Child,
    // Child::wait closes its own stdin; keep the owner lease outside Child.
    _owner: ChildStdin,
}

pub(super) fn worker(listen: String, image_store: Option<PathBuf>) -> anyhow::Result<()> {
    // A pipe held only by our owner also stops the worker if the daemon crashes.
    std::thread::Builder::new()
        .name("cache-owner".into())
        .spawn(|| {
            let mut byte = [0];
            loop {
                match std::io::Read::read(&mut std::io::stdin().lock(), &mut byte) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    _ => break,
                }
            }
            std::process::exit(0);
        })?;
    pvisor::cache::serve(
        listen,
        image_store,
        std::env::var("PVISOR_CACHE_TOKEN").ok(),
        || {
            println!("ready");
            std::io::stdout().flush()?;
            Ok(())
        },
    )
}

pub(super) async fn start(
    listen: Option<String>,
    image_store: Option<PathBuf>,
) -> anyhow::Result<RunningCache> {
    let config = pvisor::cache::CacheConfig::from_options(
        Some(pvisor::cache::CacheBackend::Server),
        listen,
        Some(false),
        image_store,
    )?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["cache-worker", "--listen", &config.location]);
    if let Some(store) = config.image_store {
        command.arg("--image-store").arg(store);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut output = BufReader::new(child.stdout.take().context("missing readiness pipe")?);
    let mut line = String::new();
    let ready = tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut line)).await;
    if !matches!(ready, Ok(Ok(_))) || line != "ready\n" {
        child.kill().await?;
        bail!("cache component failed to become ready; see component error above");
    }
    let owner = child.stdin.take().context("missing owner pipe")?;
    Ok(RunningCache {
        child,
        _owner: owner,
    })
}

pub(super) async fn wait(child: &mut Option<RunningCache>) -> anyhow::Result<()> {
    if let Some(child) = child {
        let status = child.child.wait().await?;
        bail!("cache component exited unexpectedly: {status}");
    }
    std::future::pending().await
}

pub(super) async fn stop(service: &mut RunningCache) -> anyhow::Result<()> {
    let child = &mut service.child;
    if child.try_wait()?.is_none() {
        child.start_kill()?;
    }
    child.wait().await?;
    Ok(())
}

//! Foreground, bounded pool for experimental macOS VM cold-page sharing.
use anyhow::{Context, ensure};
use clap::Parser;
use pvisor::ram_backing::{ipc::serve, resident::CompressedPool};
use std::{
    collections::BTreeMap,
    net::Shutdown,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{net::UnixListener, sync::Semaphore, task::JoinSet};

#[derive(Parser)]
#[command(
    version,
    about = "Experimental shared cold-page pool; stopping it fails dependent VMs"
)]
struct Args {
    /// Socket inside an existing private directory owned by this user.
    socket: PathBuf,
    /// Encoded payload budget in bytes; metadata has a separate object bound.
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    max_bytes: usize,
    #[arg(long, default_value_t = 8192)]
    max_objects: usize,
    #[arg(long, default_value_t = 16)]
    max_connections: usize,
    /// Connection-owned references; 32768 covers 2 GiB in 64 KiB blocks.
    #[arg(long, default_value_t = 32768)]
    max_references: usize,
}

struct SocketCleanup(PathBuf, u64, u64);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Ok(m) = self.0.symlink_metadata()
            && (m.dev(), m.ino()) == (self.1, self.2)
        {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(
        args.max_bytes > 0
            && args.max_objects > 0
            && args.max_connections > 0
            && args.max_references > 0,
        "pool budgets must be positive"
    );
    ensure!(
        args.max_connections <= Semaphore::MAX_PERMITS,
        "connection budget exceeds semaphore limit"
    );
    let socket = args
        .socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?
        .join(args.socket.file_name().context("socket filename missing")?);
    let parent = socket.parent().unwrap().metadata()?;
    ensure!(
        parent.uid() == unsafe { libc::geteuid() } && parent.mode() & 0o077 == 0,
        "socket parent must belong to this user and have private permissions"
    );
    // Bind never removes an existing endpoint. A stale socket needs explicit cleanup.
    let socket_text = socket.to_str().context("pool socket path must be UTF-8")?;
    let listener = UnixListener::bind(&socket)?;
    let m = socket.symlink_metadata()?;
    let _cleanup = SocketCleanup(socket.clone(), m.dev(), m.ino());
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let pool = Arc::new(std::sync::Mutex::new(CompressedPool::new(
        args.max_bytes,
        args.max_objects,
    )));
    let slots = Arc::new(Semaphore::new(args.max_connections));
    let mut workers = JoinSet::new();
    let mut connections = BTreeMap::new();
    let mut next = 0u64;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    println!(
        "{}",
        serde_json::json!({"ready":true,"socket":socket_text,"experimental":true})
    );
    let outcome: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result?;
                    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } { continue; }
                    let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                    let stream = stream.into_std()?;
                    stream.set_nonblocking(false)?;
                    next = next.checked_add(1).context("connection identity exhausted")?;
                    let id = next;
                    connections.insert(id, stream.try_clone()?);
                    let owner = pool.clone();
                    let references = args.max_references;
                    workers.spawn_blocking(move || {
                        let _permit = permit;
                        (id, serve(stream, owner, references))
                    });
                }
                Some(result) = workers.join_next(), if !workers.is_empty() => {
                    let (id, result) = result.context("pool worker panicked")?;
                    connections.remove(&id);
                    if let Err(error) = result { eprintln!("pool connection ended: {error}"); }
                }
                result = tokio::signal::ctrl_c() => { result?; break; }
                _ = terminate.recv() => break,
            }
        }
        Ok(())
    }
    .await;
    drop(listener);
    for stream in connections.values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    while let Some(result) = workers.join_next().await {
        let (_, result) = result.context("pool worker panicked")?;
        if let Err(error) = result {
            eprintln!("pool connection stopped: {error}");
        }
    }
    ensure!(
        pool.lock()
            .map_err(|_| anyhow::anyhow!("pool poisoned"))?
            .object_count()
            == 0,
        "disconnected references leaked"
    );
    outcome
}

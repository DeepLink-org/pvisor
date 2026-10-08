//! Cross-process shared compression/lifetime check. Not a guest VM benchmark.
use anyhow::{Context, ensure};
use pvisor::ram_backing::{
    BLOCK_BYTES,
    ipc::{PoolClient, serve},
    resident::CompressedPool,
};
use std::{
    io::{self, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("expected server or client")?;
    let path = args.next().context("expected private socket path")?;
    if mode == "server" || mode == "vm-server" {
        let vm_service = mode == "vm-server";
        let parent = Path::new(&path).parent().context("socket parent missing")?;
        ensure!(
            parent.metadata()?.permissions().mode() & 0o077 == 0,
            "socket parent must be private"
        );
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let pool = Arc::new(Mutex::new(CompressedPool::new(
            if vm_service {
                16 * 1024 * 1024
            } else {
                1024 * 1024
            },
            if vm_service { 8192 } else { 16 },
        )));
        #[cfg(target_os = "macos")]
        if std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY").is_some() {
            let measured = pool.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(25));
                // Diagnostic only: freeze payload intern/release during the query.
                let _guard = measured.lock().unwrap();
                if let Err(error) = pvisor::ram_backing::record_process_inventory_if_requested() {
                    eprintln!("experimental pool process inventory failed: {error}");
                }
            });
        }
        println!("server-ready");
        io::stdout().flush()?;
        let mut workers = vec![];
        for _ in 0..2 {
            let stream = listener.accept()?.0;
            let owner = pool.clone();
            workers.push(std::thread::spawn(move || {
                serve(stream, owner, if vm_service { 8192 } else { 2 })
            }));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("pool service panicked"))??;
        }
        ensure!(
            pool.lock().unwrap().object_count() == 0,
            "disconnected references leaked"
        );
        std::fs::remove_file(path)?;
        println!("server-empty");
    } else {
        ensure!(mode == "client", "unknown mode");
        let mut client = PoolClient::new(UnixStream::connect(path)?, Duration::from_secs(10))?;
        let input: Vec<_> = (0..BLOCK_BYTES).map(|i| (i % 251) as u8).collect();
        let first = client.put(&input)?;
        let second = client.put(&input)?;
        ensure!(first.id() == second.id(), "same content changed identity");
        ensure!(
            client.put(&input).is_err(),
            "session reference budget was ignored"
        );
        let stats = client.stats()?;
        ensure!(
            stats.objects == 1 && stats.encoded_bytes < 1024 && stats.session_references == 2,
            "dedup/budget contract failed: {stats:?}"
        );
        let id: String = first
            .id()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        println!("client-ready {id} {}", stats.encoded_bytes);
        io::stdout().flush()?;
        let mut command = String::new();
        io::stdin().read_line(&mut command)?;
        if command.trim() == "verify" {
            let mut output = vec![0; input.len()];
            client.restore(&first, &mut output)?;
            ensure!(output == input, "restore changed content");
            client.release(first)?;
            client.restore(&second, &mut output)?;
            ensure!(output == input, "release invalidated another reference");
            ensure!(
                client.stats()?.session_references == 1,
                "reference release failed"
            );
            println!("retained-ok");
            io::stdout().flush()?;
            command.clear();
            io::stdin().read_line(&mut command)?;
            ensure!(command.trim() == "exit", "expected exit");
            client.release(second)?;
        } else {
            ensure!(command.trim() == "exit", "expected verify or exit");
        }
    }
    Ok(())
}

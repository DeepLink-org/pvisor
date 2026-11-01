//! Two-VM correctness probe for physical sharing plus compressed offload.
//! Requires Linux/KVM, a fresh CASE_ROOT, and PVISOR_CASE_ROOTFS. Not a benchmark.
use anyhow::{Context, ensure};
use pvisor::{OverlayHint, PVisor, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const GUEST: &str = r#"
import mmap, pathlib, sys, time
index = int(sys.argv[1]); size = 16*1024*1024
shared = mmap.mmap(-1, size); shared[:] = b'a'*size
private = mmap.mmap(-1, size); private[:] = bytes([65+index])*size
p = pathlib.Path
p('ready').touch()
while not p('mutate').exists(): time.sleep(.05)
assert shared[:] == b'a'*size
shared[:] = bytes([113+index])*size
p('mutated').touch()
while not p('release').exists(): time.sleep(.05)
assert shared[:] == bytes([113+index])*size
assert private[:] == bytes([65+index])*size
print('shared-backing-guest-ok', index, flush=True)
"#;
fn main() -> anyhow::Result<()> {
    // Test-only runner containment: parent wrapper delegates the original budget.
    if std::env::var_os("PVISOR_KRUN_RUNNER_SPEC").is_some() {
        if let Some(root) = std::env::var_os("CASE_CGROUP") {
            let group = PathBuf::from(root).join(format!("vm-{}", std::process::id()));
            fs::create_dir(&group)?;
            fs::write(group.join("cpu.max"), "250000 100000")?;
            fs::write(group.join("cgroup.procs"), std::process::id().to_string())?;
        }
    }
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    tokio::runtime::Runtime::new()?.block_on(run())
}
async fn marker(h: &pvisor::RunHandle, p: &std::path::Path) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !p.exists() {
            ensure!(
                !h.status().state.is_terminal(),
                "guest exited: {:?}",
                h.status()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("guest marker timeout")?
}
async fn run() -> anyhow::Result<()> {
    use pvisor::ram_backing::{
        ipc::{PoolClient, serve_shared},
        shared::SharedPool,
    };
    use std::os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    };
    let root = PathBuf::from(std::env::var_os("CASE_ROOT").context("CASE_ROOT")?);
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let socket = root.join("pool.sock");
    let listener = UnixListener::bind(&socket)?;
    let pool = Arc::new(Mutex::new(SharedPool::new(32 << 20, 8192)?));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _ = serve_shared(stream.unwrap(), pool, 65536);
            });
        }
    });
    let mut probe = PoolClient::new(UnixStream::connect(&socket)?, Duration::from_secs(5))?;
    probe.enable_shared_mapping()?;
    let mut workers = Vec::new();
    let mut cycles = Vec::new();
    let mut results = Vec::new();
    let checked=tokio::time::timeout(Duration::from_secs(240),async {
        for i in 0..2 {
            let ws=root.join(format!("ws{i}")); fs::create_dir(&ws)?;
            let stage=root.join(format!("stage{i}"));
            let settings=VmSettings {rootfs:Some(std::env::var_os("PVISOR_CASE_ROOTFS").context("rootfs")?.into()),library_dir:std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(Into::into),memory_mib:8192,cpus:2,memory_pool:Some(socket.clone()),shared_backing:true,ram_compression:true,ram_backing:Some(root.join(format!("ram{i}"))),..Default::default()};
            let rt=PVisor::builder().executors(vec![Arc::new(VmExecutor::new(settings)?)]).storage(root.join(format!("storage{i}"))).overlay(OverlayHint{lower_dirs:vec![ws.clone()],stage_dir:Some(stage.clone()),..Default::default()}).build();
            let mut spec=RunSpec::process(format!("shared-backing-{i}"),"correctness",std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or_else(|_|"/usr/bin/python3".into()));
            spec.runtime.timeout_ms=Some(240_000);
            let RunInvocation::Process(p)=&mut spec.invocation;
            p.args=vec!["-c".into(),GUEST.into(),i.to_string()];p.cwd=Some("/workspace".into());p.stdout=StdioMode::Capture;p.stderr=StdioMode::Capture;
            spec.metadata.insert("pvisor.workspace".into(),json!(ws));spec.metadata.insert("pvisor.vm.overlay_target".into(),json!("/workspace"));spec.metadata.insert("pvisor.vm.guest_cwd".into(),json!("/workspace"));
            workers.push((rt.run(spec).await?,stage.join("upper")));
        }
        for (h,p) in &workers {marker(h,&p.join("ready")).await?;}
        tokio::time::timeout(Duration::from_secs(90),async {
            loop {
                match probe.put_duplicate(&vec![0x61;4096]) {
                    Ok(object)=>{let n=probe.stats()?.cross_session_objects;probe.release(object)?;if n>0 {break;}},
                    Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{}, Err(e)=>return Err(e.into())
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            } Ok::<_,anyhow::Error>(())
        }).await.context("no payload sharing observed")??;
        // Exercise still-shared mappings before COW, then modified mappings below.
        for (i,(h,_)) in workers.iter().enumerate() {
            let report=h.offload(None).await?;
            let reader=pvisor::ram_backing::CompressedRam::open(fs::File::open(&report.backing_file)?)?;
            ensure!(reader.head_id().is_some(), "no pre-COW compressed commit");
            cycles.push(json!({"vm":i,"phase":"shared","backed":report.backed_bytes}));
            h.resume().await?;
        }
        for (h,p) in &workers {fs::write(p.join("mutate"),b"go")?;marker(h,&p.join("mutated")).await?;}
        for cycle in 0..3 { for (i,(h,_)) in workers.iter().enumerate() {
            let report=h.offload(None).await?;
            let compressed=pvisor::ram_backing::CompressedRam::open(fs::File::open(&report.backing_file)?)?;
            ensure!(compressed.head_id().is_some(),"no compressed commit");
            cycles.push(json!({"vm":i,"cycle":cycle,"backed":report.backed_bytes,"allocated":compressed.allocated_bytes()?}));
            h.resume().await?;
        }}
        fs::write(workers[0].1.join("release"),b"go")?;
        let a=workers.remove(0).0.wait().await?;ensure!(a.state==RunState::Completed && a.exit_code==Some(0),"A failed: {a:?}");
        results.push(a);
        ensure!(!workers[0].0.status().state.is_terminal(),"B exited with A");
        workers[0].0.offload(None).await?;workers[0].0.resume().await?;
        fs::write(workers[0].1.join("release"),b"go")?;
        Ok::<_,anyhow::Error>(())
    }).await.context("probe deadline").and_then(|r|r);
    if checked.is_err() {
        for (h, _) in &workers {
            h.cancel();
        }
    }
    for (h, _) in workers {
        results.push(h.wait().await?);
    }
    let passed = checked.is_ok()
        && results.len() == 2
        && results.iter().all(|r| {
            r.state == RunState::Completed
                && r.exit_code == Some(0)
                && r.output
                    .stdout
                    .as_deref()
                    .unwrap_or_default()
                    .contains("shared-backing-guest-ok")
        });
    fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":passed,"error":checked.as_ref().err().map(|e|format!("{e:#}")),"cycles":cycles,"guests":results}),
        )?,
    )?;
    checked?;
    ensure!(passed, "guest integrity failed");
    Ok(())
}

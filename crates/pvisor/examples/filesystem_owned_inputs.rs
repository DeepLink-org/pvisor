//! B-FS-ENG: sealed-input factorial experiment through the normal Job runtime.
//!
//! This engineering entry omits the CLI Host service. All four conditions retain
//! PVisor, managed Job completion, journals, Run Bundles and the native runner.
use anyhow::{Context, ensure};
use clap::{Parser, Subcommand};
use pvisor::{
    ExecutionOverlayHint, NetworkDriverConfig, OverlayHint, OverlayNetMode, PVisor, RunConfig,
    RunExecutorKind, StageDurability, VmExecutor, VmSettings,
    environment_snapshot::{BaseReference, SnapshotStore},
    job_service::RuntimeJobService,
};
use pvisor_core::{RunInvocation, RunSpec};
use pvisor_overlay_core::{LayerMutability, OverlayCore, OverlayLayout};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Copy and verify source trees once, outside timed launches.
    Prepare {
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        store: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Deliberately limited argument compatibility with reference_baselines.py.
    Run {
        #[arg(long)]
        no_agent_defaults: bool,
        #[arg(long)]
        pass_env: Vec<String>,
        #[arg(long, default_value = "off")]
        overlaynet: String,
        #[arg(long, default_value = "inherit")]
        stdio: String,
        #[arg(long, default_value = "120s")]
        timeout: String,
        #[arg(long)]
        vm: bool,
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        stage: PathBuf,
        #[arg(long)]
        vm_library_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 2)]
        cpu: u16,
        #[arg(long, default_value = "16384MiB")]
        memory: String,
        #[arg(long, default_value = "checkpoint")]
        stage_durability: StageDurability,
        #[arg(last = true, required = true)]
        payload: Vec<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    store: PathBuf,
    source_rootfs: PathBuf,
    source_workspace: PathBuf,
    rootfs_owned: PathBuf,
    workspace_owned: PathBuf,
    rootfs_reference: BaseReference,
    workspace_reference: BaseReference,
    preparation_ms: f64,
}

fn publish(path: &std::path::Path, value: &impl Serialize) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn prepare(
    rootfs: PathBuf,
    workspace: PathBuf,
    store_path: PathBuf,
    manifest_path: PathBuf,
) -> anyhow::Result<()> {
    ensure!(!manifest_path.exists(), "manifest already exists");
    let started = Instant::now();
    let source_rootfs = rootfs.canonicalize()?;
    let source_workspace = workspace.canonicalize()?;
    let store = SnapshotStore::new(&store_path)?;
    let rootfs = store.import_base(&source_rootfs)?;
    let workspace = store.import_base(&source_workspace)?;
    // import_base already audits the complete owned copies before publication.
    // Runs authenticate their seal and retain GC leases; they do not rehash them.
    let manifest = Manifest {
        schema: 1,
        store: store_path.canonicalize()?,
        source_rootfs,
        source_workspace,
        rootfs_owned: rootfs.root(),
        workspace_owned: workspace.root(),
        rootfs_reference: rootfs.reference().clone(),
        workspace_reference: workspace.reference().clone(),
        preparation_ms: started.elapsed().as_secs_f64() * 1000.0,
    };
    publish(&manifest_path, &manifest)?;
    println!(
        "OWNED_INPUTS_PREPARED {}",
        serde_json::to_string(&manifest)?
    );
    Ok(())
}

async fn run(command: Command) -> anyhow::Result<i32> {
    let Command::Run {
        no_agent_defaults,
        pass_env,
        overlaynet,
        stdio,
        timeout,
        vm,
        rootfs,
        stage,
        vm_library_dir,
        cpu,
        memory,
        stage_durability,
        payload,
    } = command
    else {
        unreachable!()
    };
    ensure!(
        vm && no_agent_defaults,
        "explicit VM and no-agent-defaults required"
    );
    ensure!(
        overlaynet == "off" && stdio == "inherit",
        "only offline inherited stdio is supported"
    );
    ensure!(
        std::env::var("PVISOR_VM_FS_WRITEBACK").as_deref() == Ok("0"),
        "experiment requires explicit WB=0"
    );
    let memory_mib: u32 = memory
        .strip_suffix("MiB")
        .context("memory must use MiB")?
        .parse()?;
    let seconds: u64 = timeout
        .strip_suffix('s')
        .context("timeout must use seconds")?
        .parse()?;
    let timeout_ms = seconds.checked_mul(1000).context("timeout overflow")?;
    let manifest_path = PathBuf::from(
        std::env::var_os("PVISOR_FS_OWNED_INPUTS_MANIFEST")
            .context("owned-inputs manifest required")?,
    );
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    ensure!(manifest.schema == 1, "unsupported owned-inputs manifest");
    let source_rootfs = rootfs.canonicalize()?;
    ensure!(
        source_rootfs == manifest.source_rootfs || source_rootfs == manifest.rootfs_owned,
        "rootfs does not match prepared source"
    );
    let condition = std::env::var("PVISOR_FS_OWNED_INPUTS_CONDITION")
        .context("owned-inputs condition required")?;
    ensure!(
        ["baseline", "rootfs", "workspace", "both"].contains(&condition.as_str()),
        "invalid owned-inputs condition"
    );
    let rootfs_immutable = condition == "rootfs" || condition == "both";
    let workspace_immutable = condition == "workspace" || condition == "both";
    let store = SnapshotStore::new(&manifest.store)?;
    let root_base = store.open_base(&manifest.rootfs_reference)?;
    let workspace_base = store.open_base(&manifest.workspace_reference)?;
    ensure!(
        root_base.root() == manifest.rootfs_owned
            && workspace_base.root() == manifest.workspace_owned,
        "owned-inputs generation mismatch"
    );
    let workspace_target = std::env::current_dir()?.canonicalize()?;
    ensure!(
        workspace_target != workspace_base.root(),
        "trial workspace must be independent of owned lower"
    );
    ensure!(!stage.exists(), "stage must be fresh");
    let workspace_lower = if workspace_immutable {
        workspace_base.root()
    } else {
        workspace_target.clone()
    };
    let execution_snapshot = if workspace_immutable {
        fs::create_dir_all(stage.join("upper"))?;
        fs::create_dir(stage.join("work"))?;
        // The snapshot conditions bind an independent apply destination to a
        // sealed read baseline. Initialize and seal a normal compact stage,
        // then let runtime preparation authenticate and reopen it.
        pvisor_overlay_core::stage::begin(&stage.join("preimages"), stage_durability)?;
        let layout = OverlayLayout::with_baseline(
            vec![workspace_lower.clone()],
            workspace_target.clone(),
            Some(&workspace_lower),
        )?;
        drop(OverlayCore::new_for_layout_with_compact_preimages(
            layout,
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            Some(stage.join("preimages")),
        )?);
        pvisor_overlay_core::stage::seal(&stage.join("upper"), &stage.join("preimages"))?;
        Some(ExecutionOverlayHint {
            target: workspace_target.clone(),
            baseline_lower: Some(workspace_lower.clone()),
            excluded_paths: vec![],
        })
    } else {
        // Keep the ordinary CLI's live-baseline observation costs in controls.
        None
    };
    let settings = VmSettings {
        rootfs: Some(root_base.root()),
        library_dir: vm_library_dir,
        cpus: cpu,
        memory_mib,
        ..Default::default()
    };
    let mut executor = VmExecutor::new(settings.clone())?;
    if rootfs_immutable {
        executor = executor.with_rootfs_base(root_base.try_clone()?)?;
    }
    let overlay = OverlayHint {
        lower_dirs: vec![workspace_lower.clone()],
        lower_mutability: vec![if workspace_immutable {
            LayerMutability::Immutable
        } else {
            LayerMutability::Mutable
        }],
        stage_dir: Some(stage.clone()),
        durability: Some(stage_durability),
        execution_snapshot,
        ..Default::default()
    };
    let run_home = PathBuf::from(
        std::env::var_os("PVISOR_RUN_HOME").context("private PVISOR_RUN_HOME required")?,
    );
    fs::create_dir_all(&run_home)?;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(executor)])
        .storage(run_home.join(&run_id))
        .network(NetworkDriverConfig {
            mode: OverlayNetMode::Off,
            ..Default::default()
        })
        .overlay(overlay)
        .build();
    let mut spec = RunSpec::process(run_id.as_str(), "filesystem-owned-inputs", &payload[0]);
    spec.runtime.timeout_ms = Some(timeout_ms);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = payload[1..].to_vec();
    process.cwd = Some("/work".into());
    process.inherit_env = false;
    for key in &pass_env {
        if let Ok(value) = std::env::var(key) {
            process.env.insert(key.clone(), value);
        }
    }
    spec.metadata.insert(
        "pvisor.workspace".into(),
        serde_json::json!(workspace_target),
    );
    spec.metadata.insert(
        "pvisor.vm.overlay_target".into(),
        serde_json::json!("/work"),
    );
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), serde_json::json!("/work"));
    let mut config = RunConfig {
        vm: settings,
        ..RunConfig::default()
    };
    config.run.executor = RunExecutorKind::Vm;
    config.run.command = payload;
    config.run.workspace = Some(workspace_target.clone());
    config.run.timeout_ms = Some(timeout_ms);
    config.run.inherit_env = false;
    config.run.pass_env = pass_env;
    config.overlaynet.mode = OverlayNetMode::Off;
    let managed = RuntimeJobService::start_managed(&runtime, spec, config).await?;
    let result = managed.wait().await?;
    let receipt = serde_json::json!({
        "schema": 1, "condition": condition, "rootfs_immutable": rootfs_immutable,
        "workspace_immutable": workspace_immutable, "writeback": false, "dax": false,
        "rootfs_owned": root_base.root(), "workspace_owned": workspace_base.root(),
        "workspace_lower": workspace_lower, "workspace_snapshot": workspace_immutable,
        "workspace_frozen_baseline": workspace_immutable,
        "rootfs_reference": root_base.reference(), "workspace_reference": workspace_base.reference(),
        "workspace_apply_target": workspace_target, "manifest": manifest_path,
        "manifest_sha256": Sha256::digest(&manifest_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "rootfs_lease": true, "workspace_lease": true,
        "runtime": "PVisor/ManagedJob/Overlay/VmExecutor", "cli_host_service": false,
        "run_id": run_id, "exit_code": result.exit_code, "state": result.state,
    });
    let receipt_path = std::env::var_os("PVISOR_FS_OWNED_INPUTS_RECEIPT")
        .map(PathBuf::from)
        .unwrap_or_else(|| stage.join("owned-inputs-receipt.json"));
    publish(&receipt_path, &receipt)?;
    eprintln!("OWNED_INPUTS_RECEIPT {}", serde_json::to_string(&receipt)?);
    if let Some(failure) = result.failure {
        eprintln!("owned-inputs Job failed: {}", failure.message);
    }
    // Leases and the runtime owner remain live until native teardown and Job
    // completion have finished. Store collection is a separate caller action.
    drop((runtime, root_base, workspace_base));
    Ok(result.exit_code.unwrap_or(125))
}

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args = Args::parse();
    match args.command {
        Command::Prepare {
            rootfs,
            workspace,
            store,
            manifest,
        } => prepare(rootfs, workspace, store, manifest),
        command @ Command::Run { .. } => {
            let code = tokio::runtime::Runtime::new()?.block_on(run(command))?;
            std::process::exit(code)
        }
    }
}

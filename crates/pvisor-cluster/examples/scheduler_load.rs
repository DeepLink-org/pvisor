//! A durable single-controller history load experiment, without VM execution.
//! The scan reference is the former counts algorithm on the very same stored
//! TaskRecords; it is not an old controller or an HTTP throughput test.
use anyhow::{Context, ensure};
use clap::Parser;
use pvisor_cluster::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    hint::black_box,
    path::PathBuf,
    time::Instant,
};

#[derive(Parser)]
struct Args {
    /// Total retained records: cancelled history plus ready tasks.
    #[arg(long, value_delimiter = ',', default_value = "1000,10000,100000")]
    tasks: Vec<usize>,
    /// Ready task count per case, or "all" for a dense live queue.
    #[arg(long, default_value = "1")]
    ready: String,
    #[arg(long, default_value_t = 20)]
    samples: usize,
    #[arg(long, default_value_t = 100)]
    indexed_batch: usize,
    #[arg(long)]
    output: PathBuf,
}

fn task(id: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "scheduler-load", "/bin/true");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    TaskSpec {
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "load".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
        },
        resources: Resources {
            slots: 1,
            memory_bytes: 64 * 1024 * 1024,
            cpu_millis: 250,
        },
        labels: Default::default(),
        cache_keys: vec![],
        retain_bundle: false,
        retain_artifacts: None,
        gateway: None,
        cpu_qos: None,
        restore: None,
        environment: None,
    }
}

fn full_scan(scheduler: &Scheduler) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for task in scheduler.task_records() {
        *counts
            .entry(format!("{:?}", task.phase).to_lowercase())
            .or_default() += 1;
    }
    counts
}

fn sample(batch: usize, mut f: impl FnMut() -> BTreeMap<String, usize>) -> f64 {
    let start = Instant::now();
    for _ in 0..batch {
        black_box(f());
    }
    start.elapsed().as_nanos() as f64 / batch as f64
}

fn distribution(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        let rank = p * (values.len() - 1) as f64;
        let floor = rank.floor() as usize;
        let ceil = rank.ceil() as usize;
        values[floor] + (values[ceil] - values[floor]) * (rank - floor as f64)
    };
    json!({"unit":"ns_per_call", "n":values.len(), "minimum":values[0],
        "p50":percentile(0.5), "p95":percentile(0.95), "maximum":values[values.len()-1], "sorted_samples":values})
}

fn memory() -> Value {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return Value::Null;
    };
    let mut fields = BTreeMap::new();
    for name in ["VmRSS", "VmHWM", "VmSize"] {
        if let Some(value) = status
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}:")))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
        {
            fields.insert(format!("{name}_kib"), value);
        }
    }
    json!(fields)
}

fn source_hashes() -> anyhow::Result<BTreeMap<String, String>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut hashes = BTreeMap::new();
    for file in [
        "src/scheduler.rs",
        "src/scheduler/graph.rs",
        "src/scheduler/indexes.rs",
        "src/journal.rs",
        "examples/scheduler_load.rs",
        "Cargo.toml",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        hashes.insert(
            file.into(),
            blake3::hash(&std::fs::read(manifest.join(file))?)
                .to_hex()
                .to_string(),
        );
    }
    Ok(hashes)
}

fn experiment(total: usize, args: &Args) -> anyhow::Result<Value> {
    let ready = if args.ready == "all" {
        total
    } else {
        args.ready.parse::<usize>()?
    };
    ensure!((1..=total).contains(&ready), "ready count must be 1..tasks");
    let history = total - ready;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("wal");
    let config = SchedulerConfig {
        max_tasks: total,
        lease_duration_ms: 300_000,
        max_journal_bytes: SchedulerConfig::default()
            .max_journal_bytes
            .max(total as u64 * 4096),
        ..Default::default()
    };
    let mut scheduler = Scheduler::open(&path, config.clone())?;
    let mut ids = Vec::with_capacity(total);
    let started = Instant::now();
    for graph in 0..history.div_ceil(256) {
        let mut nodes = Vec::new();
        for index in (graph * 256)..((graph + 1) * 256).min(history) {
            let id = format!("history-{index:07}");
            nodes.push(TaskGraphNode {
                task: task(&id),
                depends_on: vec![],
            });
            ids.push(id);
        }
        let id = format!("history-graph-{graph}");
        scheduler.submit_graph(
            TaskGraphSpec {
                version: CLUSTER_VERSION,
                id: id.clone(),
                tenant: "load".into(),
                nodes,
            },
            0,
        )?;
        scheduler.cancel_graph(&id, 1)?;
    }
    for graph in 0..ready.div_ceil(256) {
        let mut nodes = Vec::new();
        for index in (graph * 256)..((graph + 1) * 256).min(ready) {
            let id = format!("ready-{index:07}");
            nodes.push(TaskGraphNode {
                task: task(&id),
                depends_on: vec![],
            });
            ids.push(id);
        }
        scheduler.submit_graph(
            TaskGraphSpec {
                version: CLUSTER_VERSION,
                id: format!("ready-graph-{graph}"),
                tenant: "load".into(),
                nodes,
            },
            2,
        )?;
    }
    let prepared_us = started.elapsed().as_micros();
    let expected: BTreeMap<_, _> = [("cancelled".into(), history), ("queued".into(), ready)]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect();
    ensure!(scheduler.counts() == expected, "prepared counts differ");
    let controller_memory = memory();
    ensure!(full_scan(&scheduler) == expected, "scan reference differs");
    let wal_bytes = std::fs::metadata(&path)?.len();
    // Warm both implementations, then interleave their batches to reduce
    // ordering bias. Indexed samples amortize clock overhead over many reads.
    black_box(scheduler.counts());
    black_box(full_scan(&scheduler));
    let mut indexed = Vec::new();
    let mut scanned = Vec::new();
    for index in 0..args.samples {
        if index % 2 == 0 {
            indexed.push(sample(args.indexed_batch, || {
                black_box(&scheduler).counts()
            }));
            scanned.push(sample(1, || full_scan(black_box(&scheduler))));
        } else {
            scanned.push(sample(1, || full_scan(black_box(&scheduler))));
            indexed.push(sample(args.indexed_batch, || {
                black_box(&scheduler).counts()
            }));
        }
    }
    ensure!(
        scheduler.counts() == expected && full_scan(&scheduler) == expected,
        "counts changed during reads"
    );
    ensure!(
        std::fs::metadata(&path)?.len() == wal_bytes,
        "counts wrote WAL"
    );
    // Walk the former FIFO/lookahead algorithm on borrowed IDs from the same
    // records. Omit WAL, renewal and admission: this is scan effort only.
    let mut queue: VecDeque<_> = ids.iter().map(String::as_str).collect();
    let phases: BTreeMap<_, _> = scheduler
        .task_records()
        .map(|task| (task.spec.id.as_str(), task.phase))
        .collect();
    let mut empty_windows = 0;
    let mut skipped = 0;
    loop {
        let mut found = false;
        for _ in 0..queue.len().min(config.queue_lookahead) {
            let id = queue
                .pop_front()
                .context("reference queue unexpectedly empty")?;
            if phases[id] == TaskPhase::Queued {
                found = true;
            } else {
                skipped += 1;
            }
        }
        if found {
            break;
        }
        empty_windows += 1;
    }
    ensure!(
        skipped == history,
        "history reference did not visit every cancelled entry"
    );
    drop(phases);
    drop(queue);
    scheduler.register(
        WorkerRegistration {
            version: CLUSTER_VERSION,
            id: "node".into(),
            incarnation: "epoch".into(),
            capacity: task("budget").resources,
            execution: vec![task("class").execution],
            labels: Default::default(),
            cache_keys: vec![],
            vm_control_protocol: None,
            vm_control_actions: vec![],
            artifact_protocol: None,
            artifact_export: None,
            gateway: None,
            cpu_observation_protocol: None,
            cpu_qos_classes: vec![],
            execution_restore_protocol: None,
            parked_execution_suspend_protocol: None,
            environment_support: None,
        },
        3,
    )?;
    let request = PollRequest {
        worker_id: "node".into(),
        incarnation: "epoch".into(),
        active: vec![],
        available: task("budget").resources,
        max_assignments: 1,
        admission: None,
    };
    let start = Instant::now();
    let response = scheduler.poll(request, 4)?;
    let poll_us = start.elapsed().as_micros();
    ensure!(
        response.assignments.len() == 1 && response.assignments[0].spec.id == "ready-0000000",
        "first poll did not assign ready task"
    );
    let key = response.assignments[0].lease.key.clone();
    scheduler.complete(
        Completion {
            key: key.clone(),
            result: None,
            error: Some("synthetic load terminal record".into()),
            artifacts: None,
            artifact_error: None,
        },
        5,
    )?;
    let final_counts: BTreeMap<_, _> = [
        ("cancelled".into(), history),
        ("failed".into(), 1),
        ("queued".into(), ready - 1),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .collect();
    ensure!(scheduler.counts() == final_counts, "terminal counts differ");
    let wal_bytes_after_completion = std::fs::metadata(&path)?.len();
    drop(scheduler);
    let start = Instant::now();
    let mut reopened = Scheduler::open(&path, config.clone())?;
    let reopen_us = start.elapsed().as_micros();
    ensure!(reopened.counts() == final_counts, "replay counts differ");
    ensure!(
        reopened
            .task("ready-0000000")?
            .lease
            .as_ref()
            .is_some_and(|lease| lease.key == key),
        "replay lost fencing identity"
    );
    let replay_memory = memory();
    // Exercise the rebuilt queue, rather than proving only aggregate replay.
    // The completed key must not reappear; equal-time queued tasks retain the
    // deterministic task-ID order used by production restart.
    let response = reopened.poll(
        PollRequest {
            worker_id: "node".into(),
            incarnation: "epoch".into(),
            active: vec![],
            available: task("budget").resources,
            max_assignments: 1,
            admission: None,
        },
        6,
    )?;
    let replay_probe_assigned_task = response
        .assignments
        .first()
        .map(|assignment| assignment.spec.id.clone());
    ensure!(
        response.assignments.len() == usize::from(ready > 1)
            && replay_probe_assigned_task.as_deref() == (ready > 1).then_some("ready-0000001"),
        "replayed queue reassigned terminal work or changed restart order"
    );
    ensure!(
        reopened.task("ready-0000000")?.phase == TaskPhase::Failed
            && reopened
                .task("ready-0000000")?
                .lease
                .as_ref()
                .is_some_and(|lease| lease.key == key),
        "replay probe changed the completed fencing identity"
    );
    let wal_bytes_after_replay_probe = std::fs::metadata(&path)?.len();
    drop(reopened);
    Ok(
        json!({"tasks":total, "cancelled_history":history, "ready_tasks_before_poll":ready,
        "prepared_us":prepared_us, "wal_bytes_before_poll":wal_bytes, "wal_bytes_after_completion":wal_bytes_after_completion,
        "wal_bytes_after_replay_probe":wal_bytes_after_replay_probe, "replay_probe_assigned_task":replay_probe_assigned_task,
        "indexed_counts":distribution(indexed), "full_task_record_scan_reference":distribution(scanned),
        "scan_reference_batches_per_sample":1, "indexed_counts_batches_per_sample":args.indexed_batch,
        "max_journal_bytes":config.max_journal_bytes, "queue_lookahead":config.queue_lookahead,
        "legacy_cancelled_entries_visited":skipped, "legacy_empty_lookahead_windows":empty_windows,
        "current_polls_until_assignment":1, "current_poll_us_including_wal_and_admission":poll_us,
        "final_counts":final_counts, "reopen_us":reopen_us, "replay_fencing_verified":true,
        "controller_plus_id_fixture_memory_before_reference":controller_memory,
        "memory_after_replay_with_fixture":replay_memory}),
    )
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(
        !args.tasks.is_empty()
            && args
                .tasks
                .iter()
                .all(|count| (2..=1_000_000).contains(count)),
        "tasks must be 2..1000000"
    );
    ensure!(
        (2..=1000).contains(&args.samples) && (1..=10000).contains(&args.indexed_batch),
        "invalid sample or batch count"
    );
    ensure!(!args.output.exists(), "output already exists");
    let sources_before = source_hashes()?;
    let mut rows = Vec::new();
    for total in &args.tasks {
        eprintln!("scheduler-load: preparing {total} retained records");
        rows.push(experiment(*total, &args)?);
        eprintln!("scheduler-load: verified {total} records, counts and durable replay");
    }
    ensure!(
        source_hashes()? == sources_before,
        "source identity changed during measurement; refusing to publish results"
    );
    let executable_hash = blake3::hash(&std::fs::read(std::env::current_exe()?)?)
        .to_hex()
        .to_string();
    let report = json!({"schema":"pvisor-controller-history-load/v3", "scope":"single-controller durable typed API; synthetic task graphs; no execution",
        "protocol_version":CLUSTER_VERSION, "build_has_debug_assertions":cfg!(debug_assertions),
        "recorded_at_unix_ms":pvisor_core::unix_now_ms(), "source_blake3":sources_before, "executable_blake3":executable_hash,
        "source_identity_unchanged_during_measurement":true,
        "rust_type_size_bytes":{"task_record":std::mem::size_of::<TaskRecord>(), "boxed_task_record":std::mem::size_of::<Box<TaskRecord>>(),
            "task_spec":std::mem::size_of::<TaskSpec>(), "run_spec":std::mem::size_of::<RunSpec>()},
        "cpu_model":std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| text.lines().find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))),
        "affinity":std::fs::read_to_string("/proc/self/status").ok().and_then(|text| text.lines().find_map(|line| line.strip_prefix("Cpus_allowed_list:\t").map(str::to_owned))),
        "filesystem_note":"temporary local WAL; fsync issued; storage medium and host load affect timings",
        "reference_note":"former counts algorithm on the same authoritative TaskRecords without cloning; no former controller throughput claim; cancelled-window reference omits WAL and admission",
        "memory_note":"whole process RSS/HWM includes controller, ID fixture, allocator retention and transient reference indexes; HWM accumulates across phases/rows; use a separate process per size and mode for comparisons; replay uses a warm local WAL",
        "rows":rows});
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)?;
    output.write_all(&serde_json::to_vec_pretty(&report)?)?;
    output.sync_all()?;
    Ok(())
}

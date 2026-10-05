//! Optional bounded telemetry, independent of lease/control delivery.
use pvisor::RunControlHandle;
use pvisor_cluster::{
    AttemptMemorySample, LeaseKey, MemoryReportRequest, NodeMemoryReportRequest,
    WorkerRegistration, client::Client,
};
use pvisor_core::memory::{MemoryObservation, NodeMemorySample};
use std::time::Duration;
use tokio::{sync::watch, time::Instant};

#[derive(Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub enabled: bool,
    pub interval_ms: u64,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_ms: 5000,
        }
    }
}
#[derive(Clone)]
pub struct Target {
    pub key: LeaseKey,
    pub controls: RunControlHandle,
}

fn node_sample() -> NodeMemorySample {
    let sampled_at_unix_ms = pvisor_core::unix_now_ms();
    #[cfg(target_os = "linux")]
    let supervisor = pvisor::sample_supervisor_memory();
    #[cfg(not(target_os = "linux"))]
    let supervisor = Err(anyhow::anyhow!(
        "supervisor memory observations require Linux"
    ));
    NodeMemorySample {
        sampled_at_unix_ms,
        supervisor: MemoryObservation::from_result(supervisor),
        system: MemoryObservation::from_result(
            pvisor_cluster::physical_memory::sample_system_memory(),
        ),
        cgroup: MemoryObservation::from_result(
            pvisor_cluster::physical_memory::sample_cgroup_memory(),
        ),
    }
}

pub fn start(
    client: Client,
    profile: &Profile,
    registration: &WorkerRegistration,
) -> anyhow::Result<watch::Sender<Vec<Target>>> {
    anyhow::ensure!(
        (1000..=60_000).contains(&profile.interval_ms),
        "memory sampling interval must be 1000..60000 ms"
    );
    let (tx, mut rx) = watch::channel(Vec::<Target>::new());
    let interval = Duration::from_millis(profile.interval_ms);
    let worker_id = registration.id.clone();
    let incarnation = registration.incarnation.clone();
    tokio::spawn(async move {
        let mut node_sequence = 0_u64;
        let mut node_supported = true;
        let mut vm_supported = true;
        let mut sequences = std::collections::BTreeMap::<String, (LeaseKey, u64)>::new();
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {},
                changed = rx.changed() => { if changed.is_err() { break; } continue; },
            }
            let targets = rx.borrow_and_update().clone();
            let live: std::collections::BTreeSet<_> =
                targets.iter().map(|t| &t.key.task_id).collect();
            sequences.retain(|id, _| live.contains(id));
            if node_supported {
                let started = Instant::now();
                let sample = match tokio::task::spawn_blocking(node_sample).await {
                    Ok(sample) => sample,
                    Err(error) => {
                        let error: String = format!("node probe failed: {error}")
                            .chars()
                            .take(256)
                            .collect();
                        NodeMemorySample {
                            sampled_at_unix_ms: pvisor_core::unix_now_ms(),
                            supervisor: MemoryObservation::Unavailable {
                                error: error.clone(),
                            },
                            system: MemoryObservation::Unavailable {
                                error: error.clone(),
                            },
                            cgroup: MemoryObservation::Unavailable { error },
                        }
                    }
                };
                let Some(sequence) = node_sequence.checked_add(1) else {
                    return;
                };
                node_sequence = sequence;
                let request = NodeMemoryReportRequest {
                    worker_id: worker_id.clone(),
                    incarnation: incarnation.clone(),
                    sequence,
                    sample_age_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    sample,
                };
                if let Err(error) = client.report_node_memory(&request).await {
                    if error
                        .downcast_ref::<reqwest::Error>()
                        .and_then(|e| e.status())
                        == Some(reqwest::StatusCode::NOT_FOUND)
                    {
                        eprintln!("controller lacks node memory endpoint; node sampling stopped");
                        node_supported = false;
                    } else {
                        eprintln!("node memory observation delivery failed: {error:#}");
                    }
                }
            }
            if !vm_supported {
                if !node_supported {
                    return;
                }
                continue;
            }
            // One process probe and one upload at a time. Slow reads or HTTP
            // never hold the execution loop or generate replacement probes.
            for batch in targets.chunks(64) {
                let mut pending = Vec::with_capacity(batch.len());
                for target in batch {
                    if rx.has_changed().is_err() {
                        return;
                    }
                    let started = Instant::now();
                    let sample = target.controls.memory_sample().await;
                    let counter = sequences
                        .entry(target.key.task_id.clone())
                        .or_insert_with(|| (target.key.clone(), 0));
                    if counter.0 != target.key {
                        *counter = (target.key.clone(), 0);
                    }
                    let Some(sequence) = counter.1.checked_add(1) else {
                        return;
                    };
                    counter.1 = sequence;
                    pending.push((target.key.clone(), counter.1, started, sample));
                }
                let samples = pending
                    .into_iter()
                    .map(|(key, sequence, started, sample)| AttemptMemorySample {
                        key,
                        sequence,
                        sample_age_ms: started.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                        sample,
                    })
                    .collect();
                let request = MemoryReportRequest {
                    worker_id: batch[0].key.worker_id.clone(),
                    incarnation: batch[0].key.incarnation.clone(),
                    samples,
                };
                if let Err(error) = client.report_memory(&request).await {
                    if error
                        .downcast_ref::<reqwest::Error>()
                        .and_then(|e| e.status())
                        == Some(reqwest::StatusCode::NOT_FOUND)
                    {
                        eprintln!("controller lacks memory telemetry endpoint; sampling stopped");
                        vm_supported = false;
                        break;
                    }
                    eprintln!("memory observation delivery failed: {error:#}");
                }
            }
        }
    });
    Ok(tx)
}

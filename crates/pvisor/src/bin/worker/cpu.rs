//! Bounded CPU telemetry has its own task; slow uploads cannot hold lease/control
//! delivery or force memory mapping walks. No overlapping/replacement probes.
use super::memory::Target;
use pvisor_cluster::{AttemptCpuSample, CpuReportRequest, WorkerRegistration, client::Client};
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

pub fn start(
    client: Client,
    profile: &Profile,
    registration: &WorkerRegistration,
) -> anyhow::Result<watch::Sender<Vec<Target>>> {
    anyhow::ensure!(
        (1000..=60_000).contains(&profile.interval_ms),
        "CPU sampling interval must be 1000..60000 ms"
    );
    let (tx, mut rx) = watch::channel(Vec::<Target>::new());
    let worker_id = registration.id.clone();
    let incarnation = registration.incarnation.clone();
    let interval = Duration::from_millis(profile.interval_ms);
    tokio::spawn(async move {
        let mut sequences =
            std::collections::BTreeMap::<String, (pvisor_cluster::LeaseKey, u64)>::new();
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {},
                changed = rx.changed() => { if changed.is_err() { return; } continue; },
            }
            let targets = rx.borrow_and_update().clone();
            let live: std::collections::BTreeSet<_> =
                targets.iter().map(|t| &t.key.task_id).collect();
            sequences.retain(|id, _| live.contains(id));
            for batch in targets.chunks(64) {
                let mut pending = Vec::with_capacity(batch.len());
                for target in batch {
                    if rx.has_changed().is_err() {
                        return;
                    }
                    let began = Instant::now();
                    let sample = target.controls.cpu_sample().await;
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
                    pending.push((target.key.clone(), sequence, began, sample));
                }
                let samples = pending
                    .into_iter()
                    .map(|(key, sequence, began, sample)| AttemptCpuSample {
                        key,
                        sequence,
                        sample_age_ms: began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                        sample,
                    })
                    .collect();
                let request = CpuReportRequest {
                    worker_id: worker_id.clone(),
                    incarnation: incarnation.clone(),
                    samples,
                };
                if let Err(error) = client.report_cpu(&request).await {
                    if error
                        .downcast_ref::<reqwest::Error>()
                        .and_then(|e| e.status())
                        == Some(reqwest::StatusCode::NOT_FOUND)
                    {
                        eprintln!("controller lacks CPU endpoint; CPU sampling stopped");
                        return;
                    }
                    eprintln!("CPU observation delivery failed: {error:#}");
                }
            }
        }
    });
    Ok(tx)
}

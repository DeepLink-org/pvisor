//! B-LAZY-STARTUP cold client then immediate hot client, randomized by pair.
use super::{
    image_compare::{Cohort, GROUPS},
    process::{self, save},
};
use anyhow::Result;
use criterion::Criterion;
use serde_json::json;
use std::{fs, time::Duration};

pub(super) fn block(cohort: &mut Cohort, phase: &str, metric: &str) -> Result<Duration> {
    let index = cohort.block;
    cohort.block += 1;
    let order = cohort.schedule.next();
    save(
        &cohort.config.output.join(format!("order-{index:06}.json")),
        &json!({"phase":phase,"block":index,"order":order}),
    )?;
    let before = process::snapshot(&[cohort.vm.pid()])?;
    let mut sum = 0u64;
    for condition in order {
        let group = GROUPS[condition];
        let key = format!("{phase}-{index:06}-{group}");
        let pair = cohort.config.output.join("pairs").join(&key);
        fs::create_dir(&pair)?;
        if group != "pvisor" {
            cohort.lab.reset_client(&pair)?;
        }
        let mut results = Vec::new();
        for (state, warm) in [("cold", false), ("warm", true)] {
            let id = format!("{key}-{state}");
            let result = cohort.trial(group, &id, true, warm, &key)?;
            results.push(result.clone());
            let measured = result[metric]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("missing selected process duration"))?;
            cohort.record(phase, index, group, state, result)?;
            sum += measured;
        }
        save(
            &pair.join("pair.json"),
            &json!({"valid":true,"cache_key":key,"order":["cold","warm"],"results":results,
            "cache":"cold private client then same persistent client cache, fresh tasks/VMs and writable state",
            "preserved":"server artifacts and host page cache; OverlayBD service processes restarted outside measured intervals"}),
        )?;
        cohort.lab.audit_inventory(&pair)?;
    }
    process::background(
        &before,
        &process::snapshot(&[cohort.vm.pid()])?,
        &cohort.config.cpus,
        &cohort
            .config
            .output
            .join(format!("background-{index:06}.json")),
    )?;
    Ok(Duration::from_nanos(sum))
}
pub(super) fn run(criterion: &mut Criterion) -> Result<()> {
    let mut cohort = Cohort::prepare("cold-warm-client")?;
    let outcome = super::image_compare::measure(criterion, &mut cohort, true);
    let finish = cohort.finish(&outcome);
    outcome?;
    finish
}

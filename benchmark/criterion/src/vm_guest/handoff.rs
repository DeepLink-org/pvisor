//! B-VM-GUEST-ENG: same-runner off/forced/auto diagnostics, outside the regression gate.
use super::{Subjects, duration, pvisor_command, rootfs};
use crate::filesystem::support::{Rng, records, run, save, sha};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{fmt::Write, fs, path::Path, time::Duration};

const MODES: [&str; 3] = ["0", "1", "auto"];

fn child_cpu_us() -> Result<i64> {
    let mut usage = std::mem::MaybeUninit::uninit();
    // SAFETY: getrusage initializes the supplied buffer on success.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) };
    ensure!(rc == 0, "getrusage: {}", std::io::Error::last_os_error());
    let usage = unsafe { usage.assume_init() };
    Ok([usage.ru_utime, usage.ru_stime]
        .iter()
        .map(|t| t.tv_sec * 1_000_000 + t.tv_usec)
        .sum())
}

pub(super) fn compare(dir: &Path, candidate: &Subjects, spacing_probe: &Path) -> Result<()> {
    fs::create_dir(dir)?;
    const SEED: u64 = 21341;
    let mut rng = Rng(SEED);
    save(
        dir.join("inputs.json"),
        &json!({
            "seed": SEED, "runner": sha(&candidate.vm)?, "gate_probe": sha(&candidate.guest)?,
            "spacing_probe": sha(spacing_probe)?, "vcpus": 1, "memory_mib": 256
        }),
    )?;
    let mut report = String::from(concat!(
        "### Candidate queue handoff (diagnostic, not the gate)\n\n",
        "Medians over 15 fresh-VM blocks, same binary. Read time excludes think time; ",
        "CPU includes the whole VMM lifetime. Path counters come from a separate VM, including setup.\n\n",
        "| Workload | Requested | Guest ms | VMM CPU ms | Negotiated | Worker hits / misses | Inline metadata |\n",
        "|---|---|---:|---:|---|---:|---:|\n",
    ));
    for metric in [
        "disk-warm-read",
        "disk-shuffled-read",
        "disk-write-fsync",
        "disk-shuffled-overwrite-fsync",
        "dense",
        "spaced",
    ] {
        let probe = if matches!(metric, "dense" | "spaced") {
            spacing_probe
        } else {
            &candidate.guest
        };
        let mut samples = [[(0u64, 0i64); 15]; 3];
        // Block 15 verifies selected paths separately. Never time the profile as a trial.
        for block in 0..=15 {
            let mut order = [0, 1, 2];
            if block < 15 {
                rng.shuffle(&mut order);
            }
            for arm in order {
                let mode = MODES[arm];
                let logs = dir.join(metric).join(format!("{block}-{mode}"));
                let temp = tempfile::tempdir_in(dir)?;
                let root = temp.path().join("root");
                rootfs(&root, probe)?;
                let mut cmd = pvisor_command(&candidate.vm, &root, "rust-native", 1);
                cmd.env("PVISOR_VM_FS_QUEUE_HANDOFF", mode)
                    .args(["--", "/probe", metric]);
                if block == 15 {
                    cmd.env("PVISOR_FS_PROFILE", "1");
                }
                let before = child_cpu_us()?;
                let (wall, stdout) = run(&mut cmd, &logs, Duration::from_secs(30), &[])?;
                let cpu = child_cpu_us()? - before;
                let elapsed = duration(&stdout, metric)?;
                ensure!(elapsed <= wall && cpu > 0, "invalid handoff trial");
                if block < 15 {
                    let ns = elapsed.as_nanos() as u64;
                    samples[arm][block] = (ns, cpu);
                    save(
                        logs.join("result.json"),
                        &json!({"mode":mode,"guest_ns":ns,"whole_vm_cpu_us":cpu}),
                    )?;
                    continue;
                }
                let stderr = fs::read_to_string(logs.join("stderr.log"))?;
                let profiles = records(&stderr, "pvisor-fs-profile ")?;
                let counters = &profiles
                    .iter()
                    .find(|r| r["final_record"] == true && r["component"] == "virtio-fs-dispatch")
                    .context("no final dispatch profile")?["measurements"];
                let units = |k: &str| counters[k]["units"].as_u64().unwrap_or(0);
                let selected = match (units("handoff_auto_queues"), units("handoff_forced_queues"))
                {
                    (0, 0) => "off/fallback",
                    (1.., 0) if mode == "auto" => "auto",
                    (0, 1..) if mode == "1" => "forced",
                    _ => anyhow::bail!("unexpected handoff negotiation: {mode} {counters}"),
                };
                let mut times = samples[arm].map(|s| s.0);
                times.sort_unstable();
                let mut cpus = samples[arm].map(|s| s.1);
                cpus.sort_unstable();
                writeln!(
                    report,
                    "| {metric} | {mode} | {:.3} | {:.3} | {selected} | {} / {} | {} |",
                    times[7] as f64 / 1e6,
                    cpus[7] as f64 / 1e3,
                    units("handoff_hit"),
                    units("handoff_miss"),
                    units("vcpu_inline_metadata")
                )?;
            }
        }
    }
    fs::write(dir.join("summary.md"), report)?;
    Ok(())
}

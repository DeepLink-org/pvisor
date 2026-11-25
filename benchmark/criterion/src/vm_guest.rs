//! Benchmark: B-VM-GUEST-ENG (benchmark/REGISTRY.md#b-vm-guest-eng), role engineering A/B.
//! Motivation: catch guest CPU, memory, storage and tmpfs regressions.
//! Conclusion sought: a per-kernel verdict per workload, with a fixed Firecracker reference.
//! Design: identical guest probe, fresh writable backing, guest clock and verified results.
//! Benchmark: B-VM-STARTUP-ENG (benchmark/REGISTRY.md#b-vm-startup-eng), role engineering A/B.
//! Measure Create → Running; guest output and exit are correctness checks, not stop events.
use crate::{
    command::measured_total,
    filesystem::support::{checked, clean, output, required, run, save, sha},
    vm_matrix::{self, GROUP, REFERENCE},
};
use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

struct Subjects {
    vm: PathBuf,
    firmware: PathBuf,
    guest: PathBuf,
    fc: PathBuf,
    kernel: PathBuf,
}

fn rootfs(root: &Path, guest: &Path) -> Result<()> {
    fs::create_dir(root)?;
    for dir in ["dev", "ram", "work"] {
        fs::create_dir(root.join(dir))?;
    }
    fs::copy(guest, root.join("probe"))?;
    Ok(())
}

fn duration(stdout: &str, metric: &str) -> Result<Duration> {
    let mut lines = stdout.lines().filter(|s| s.starts_with("VM_GUEST_OK"));
    let line = lines.next().context("missing guest completion record")?;
    let ns: u64 = line
        .strip_prefix(&format!("VM_GUEST_OK {metric} "))
        .context("wrong guest metric")?
        .parse()?;
    ensure!(ns > 0 && lines.next().is_none(), "invalid guest result");
    Ok(Duration::from_nanos(ns))
}

impl Subjects {
    fn operation(
        &self,
        batch: &Path,
        logs: &Path,
        backend: &str,
        metric: &str,
    ) -> Result<Duration> {
        let startup = metric == "direct-running";
        let cpus = if startup { 4 } else { 1 };
        fs::create_dir_all(logs)?;
        let backing = tempfile::tempdir_in(batch)?;
        let root = backing.path().join("root");
        let mut cmd;
        if backend == REFERENCE {
            let image = backing.path().join("root.ext4");
            checked(
                Command::new("cp")
                    .args(["--reflink=auto", "--sparse=always"])
                    .arg(batch.join("template.ext4"))
                    .arg(&image),
            )?;
            let config = logs.join("config.json");
            save(
                &config,
                &json!({
                    "boot-source":{"kernel_image_path":self.kernel, "boot_args":format!(
                        "console=ttyS0 reboot=k panic=1 quiet pci=off rw root=/dev/vda init=/probe BENCH_METRIC={metric}")},
                    "machine-config":{"vcpu_count":cpus,"mem_size_mib":256},
                    "drives":[{"drive_id":"rootfs","path_on_host":image,
                        "is_root_device":true,"is_read_only":false,"cache_type":"Writeback"}],
                }),
            )?;
            cmd = Command::new(&self.fc);
            clean(&mut cmd);
            cmd.args(["--no-api", "--config-file"]).arg(config);
        } else {
            rootfs(&root, &self.guest)?;
            cmd = Command::new(&self.vm);
            clean(&mut cmd);
            cmd.env("PVISOR_GUEST_KERNEL", backend)
                .arg("--rootfs")
                .arg(&root)
                .arg("--cpus")
                .arg(cpus.to_string())
                .args(["--memory", "256", "--task-stdio"]);
            if backend == "linux" {
                cmd.arg("--firmware").arg(&self.firmware);
            }
            cmd.args(["--", "/probe", metric]);
        }
        if startup {
            use crate::startup::DirectVmm;
            let vmm = if backend == REFERENCE {
                DirectVmm::Firecracker
            } else {
                DirectVmm::Pvisor
            };
            return vmm.create_to_running(cmd, logs);
        }
        let (wall, stdout) = run(&mut cmd, logs, Duration::from_secs(30), &[])?;
        let elapsed = duration(&stdout, metric)?;
        ensure!(elapsed <= wall, "guest duration exceeds VM lifetime");
        save(
            logs.join("result.json"),
            &json!({"backend":backend,"metric":metric,"guest_ns":elapsed.as_nanos()}),
        )?;
        Ok(elapsed)
    }
}

pub fn register(c: &mut Criterion, startup: bool) -> Result<()> {
    let batch = output()?;
    let subjects = Subjects {
        vm: required("PVISOR_BENCH_BINARY")?,
        firmware: required("PVISOR_BENCH_LINUX_FW")?,
        guest: required("PVISOR_BENCH_GUEST")?,
        fc: required("PVISOR_BENCH_FIRECRACKER")?,
        kernel: required("PVISOR_BENCH_FC_KERNEL")?,
    };
    let backing_fs = checked(Command::new("stat").args(["-f", "-c", "%T"]).arg(&batch))?;
    ensure!(
        matches!(backing_fs.trim(), "ext2/ext3" | "btrfs" | "xfs"),
        "use disk-backed ext4, btrfs or XFS for --output, got {backing_fs}"
    );
    let status = fs::read_to_string("/proc/self/status")?;
    save(
        batch.join("manifest.json"),
        &json!({
            "benchmark":if startup {"B-VM-STARTUP-ENG"} else {"B-VM-GUEST-ENG"},
            "vcpus":if startup {4} else {1},"memory_mib":256,
            "backing_fs":backing_fs.trim(),"host":checked(Command::new("uname").arg("-a"))?.trim(),
            "affinity":status.lines().find(|s| s.starts_with("Cpus_allowed_list:")),
            "harness_sha256":sha(&std::env::current_exe()?)?,
            "vm_sha256":sha(&subjects.vm)?,"firmware_sha256":sha(&subjects.firmware)?,
            "guest_sha256":sha(&subjects.guest)?,"firecracker_sha256":sha(&subjects.fc)?,
            "fc_kernel_sha256":sha(&subjects.kernel)?
        }),
    )?;
    fs::write(
        batch.join("probe.c"),
        if startup {
            include_str!("../../vm/startup_probe.c")
        } else {
            include_str!("../../vm/probe.c")
        },
    )?;
    fs::write(batch.join("vm_guest.rs"), include_str!("vm_guest.rs"))?;
    let template = batch.join("template.ext4");
    let seed = tempfile::tempdir_in(&batch)?;
    let root = seed.path().join("root");
    rootfs(&root, &subjects.guest)?;
    fs::File::create_new(&template)?.set_len(32 << 20)?;
    checked(
        Command::new("mke2fs")
            .args(["-q", "-F", "-t", "ext4", "-d"])
            .arg(root)
            .arg(&template),
    )?;

    let mut group = c.benchmark_group(if startup {
        vm_matrix::STARTUP_GROUP
    } else {
        GROUP
    });
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(1));
    let cells: Vec<_> = if startup {
        vm_matrix::BACKENDS
            .into_iter()
            .map(|b| (b, "direct-running", format!("{b}-direct-running")))
            .collect()
    } else {
        vm_matrix::cells().collect()
    };
    for (backend, metric, name) in cells {
        let logs = batch.join(&name);
        let mut index = 0u64;
        group.bench_function(name, |b| {
            b.iter_custom(|iters| {
                measured_total(iters, || {
                    index += 1;
                    let trial = logs.join(index.to_string());
                    subjects
                        .operation(&batch, &trial, backend, metric)
                        .with_context(|| format!("see {}", trial.display()))
                })
                .expect("valid guest measurements")
            })
        });
    }
    if startup {
        crate::startup::startup_hits(&mut group, &batch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_requires_one_matching_positive_duration() {
        assert_eq!(
            duration("boot\nVM_GUEST_OK cpu-integer 23\r\n", "cpu-integer").unwrap(),
            Duration::from_nanos(23)
        );
        for text in [
            "",
            "VM_GUEST_OK tmpfs-read 23",
            "VM_GUEST_OK cpu-integer 0",
            "VM_GUEST_OK cpu-integer -1",
            "VM_GUEST_OK cpu-integer 2\nVM_GUEST_OK cpu-integer 3",
        ] {
            assert!(duration(text, "cpu-integer").is_err());
        }
    }
}

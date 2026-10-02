# Startup latency

Covers cold start, warm start, and resident memory; the control groups are bare processes, `docker run`, Firecracker, and each agent's built-in sandbox; the workload is `pvisor run -- true`, across five configurations: host, host+stage, `--safe`, container, and VM.

## Migrated data: VM executor guest init readiness latency (Apple Silicon)

These results measure only the phase from guest init to ready inside the VM executor, comparing C and Rust guest init implementations; they are not pVisor's end-to-end cold-start latency and do not cover the host or container executors. Source: `benchmark/pvisor/README.md`. Measured 2026-10-01 on Apple M4/HVF, libkrunfw 5.5.0, Alpine minirootfs 3.22.1 aarch64, 10 warmups / 100 samples:

| Scenario | C ready p50 (ms) | Rust ready p50 (ms) | Rust ready p95 (ms) | p50 change |
| --- | ---: | ---: | ---: | ---: |
| Direct command, networking off | 105.27 | 114.57 | 116.63 | 8.83% slower |
| Workspace, networking off | 119.18 | 114.48 | 116.24 | 3.94% faster |
| Workspace, networking on | 119.46 | 114.81 | 116.33 | 3.89% faster |

The method, scripts, and original reports remain under `benchmark/pvisor/`.

!!! note "Under construction"
    Cold start, warm start, and resident memory for the five host / host+stage / --safe / container / VM configurations, plus p99 and sample counts, have no data yet; reporting requirements are in [methodology](methodology.md).
    These results describe only this HVF runner and do not extrapolate to Linux/KVM.

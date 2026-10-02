# Startup latency

The measurement plan covers cold/warm startup and resident memory, comparing bare processes, `docker run`, Firecracker and agent-native sandboxes. The workload is `pvisor run -- true`, covering host, host+stage, `--safe`, container and VM configurations.

## Migrated data: VM guest init readiness latency (Apple Silicon)

These results measure only the VM guest init-to-ready phase, comparing C and Rust guest init implementations. They are not end-to-end pVisor cold-start measurements and do not cover host/container executors. Source: `benchmark/pvisor/README.md`. Measured 2026-10-01 on Apple M4/HVF, libkrunfw 5.5.0, Alpine minirootfs 3.22.1 aarch64, with 10 warmups and 100 samples:

| Scenario | C ready p50 (ms) | Rust ready p50 (ms) | Rust ready p95 (ms) | p50 change |
| --- | ---: | ---: | ---: | ---: |
| Direct command, networking off | 105.27 | 114.57 | 116.63 | 8.83% slower |
| Workspace, networking off | 119.18 | 114.48 | 116.24 | 3.94% faster |
| Workspace, networking on | 119.46 | 114.81 | 116.33 | 3.89% faster |

Method, scripts and original reports remain under `benchmark/pvisor/`.

!!! note "TODO"
    Add cold/warm startup and resident-memory measurements for host, host+stage, --safe and container configurations. Add p99 and sample counts; limit conclusions to this HVF runner rather than extrapolating to Linux/KVM.

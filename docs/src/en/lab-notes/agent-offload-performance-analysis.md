# Memory offload and resume during Agent waits

This engineering experiment asks how much VM memory pause and offload can reclaim during model waits, and what execution cost they add. The implementation is from [PR #33](https://github.com/DeepLink-org/pvisor/pull/33). Results use frozen SWE trajectory-replay builds, not a performance guarantee for the current release or production clusters.

## The problem: idle CPU, resident memory {#motivation}

Agents alternate model requests with tool execution. While waiting for external responses, VMs retain processes, file caches and execution context. Low CPU utilization therefore does not imply low memory occupancy; memory can constrain concurrency before CPU does. Overcommit makes allocation more flexible, but does not identify Agent wait windows or replace resident-memory reclamation.

The following preliminary observations are retained from three SWE task groups. Task identities, sample counts and aggregation definitions remain to be supplied. They are separate from the fixed replay below and do not establish a universal bottleneck. API waits include service queuing, generation and network time, not just model reasoning.

| Metric | Group 1 | Group 2 | Group 3 |
| --- | ---: | ---: | ---: |
| API waiting share of Agent execution time | 76.00% | 69.00% | 44.80% |
| Mean VM cgroup CPU during API waits (cores) | 0.235 | 0.267 | 0.295 |
| Mean VM cgroup memory during API waits (MiB) | 763 | 591 | 2252 |

## Three wait signals and Host-owned I/O {#wait-scenarios}

OverlayNet and Gateway expose network and model-request waits; the virtio-fs path backed by OverlayCore exposes backend file-read waits. The Host can advance these operations independently. A pending request is only a hint: pausing also requires guest idle evidence and device-access checks.

| Scenario | Host signal | Work that continues while paused | Resume trigger |
| --- | --- | --- | --- |
| Model wait | Execution-bound Gateway request not yet delivered | Await upstream; buffer streaming bytes within configured bounds | Deliverable data, byte/time batch limit, EOF or error |
| Download wait | Authorized GET awaiting headers/chunks, or CONNECT awaiting downstream data | Receive and prepare forwarding, preserving backpressure | Next chunk, EOF or error |
| Lazy image read | virtio-fs READ or supported read-only OPEN awaiting Host backend | Fetch, validate and hold content in Host buffers | Backend completion; resume before writing the guest response |

All three feed wait-start and data-ready events into one WaitCoordinator. Guest writes during downloads, file writes and copy-up do not automatically qualify for offload. The performance experiment below enables model-wait management only; it does not measure combined download, lazy-image or streaming-buffer benefits.

## Pause, Offload and Resume {#operations}

| Operation | Effect | Boundary |
| --- | --- | --- |
| Pause | Stop vCPU execution | Usually retains memory; does not mean devices have stopped accessing RAM |
| Offload | Synchronize RAM to backing storage and request resident-page reclamation | Requires pause and blocking/draining relevant device RAM access |
| Resume | Continue the original VM and device access | Later accesses may fault pages back in; does not create a VM from a checkpoint |

The common path is Host wait detection → guest idle handoff → pause → conditional offload → data or a real event arrives → resume and deliver. Short waits can pause without offloading; reclamation additionally requires timer budget and cooldown. Host I/O progress does not permit writing guest RAM while device access is gated.

File-level management exposes boundaries at which some backend I/O can be taken over. RAM backing is a file storing guest memory contents, a different kind of state. See [memory offload](../design/memory-optimization/offload.md) for details.

## Host and guest-kernel cooperation {#kernel-support}

Policy resides on the Host; the guest kernel supplies actual idle evidence. No special Agent headers or model-message changes are required. This records the PR's Linux x86_64/KVM, single-vCPU path: experimental, explicitly configured and disabled by default.

| Component | Implementation | Preserved semantics and limits |
| --- | --- | --- |
| Host coordinator | Wait lifecycle, repeated checks, cooldown, pre-registration before resume and delivery hold | Manual-pause ownership, cancellation and stale-epoch checks |
| VMM and virtio-fs | RAM gate; long READ releases RAM lease and holds results on Host | Reacquire access before delivery; supported read-only paths only |
| 0050 idle handoff | Report actual clockevent, distinguishing timer and no-timer cases | Real interrupts and timers still wake the VM |
| 0051/0054 fair server | Fix idle stopping and timer-callback requeueing | Scheduling budget, time accounting and normal wakeups |
| 0052/0053 PSI | Avoid unnecessary restarts after pressure-free idle; safely defer aggregation | Preserve accounting/triggers and catch up at original timestamps; restricted single-core conditions |
| 0055 network watchdog | Defer eligible empty checks with no outstanding transmission; restore expiry after handoff | No NIC shutdown, packet loss or deferral of TCP/application timers |

[PR #33](https://github.com/DeepLink-org/pvisor/pull/33) documents code and kernel boundaries. These mechanisms do not imply equivalent performance validation across all three wait scenarios.

## SWE trajectory replay: configuration and results {#results}

### Configuration {#configuration}

Both cohorts replay the same `qutebrowser-667` trajectory: 44 fixed model responses with 232.749 seconds of fixed waits. Tool commands execute in real VMs; no live model is called. Checks compare commands, return codes and final patch with the source trajectory and run task verification. Replay success is not a new model solve-rate result.

| Setting | Single VM | 16 concurrent VMs |
| --- | --- | --- |
| Host | gpu02, Linux/KVM | Same Host |
| Per VM | 1 vCPU, 7 GiB guest RAM | Same |
| Service budget | One-core CPU quota, 9 GiB memory limit, no swap | Same; parent cgroup 64 GiB, no swap |
| Conditions and samples | Baseline/pause once each; later SSD offload three times | Baseline/pause/offload: one batch of 16 each |
| Startup and cache | Fresh VM/private writes; reuse read-only base, no global cache drop | All ready and observers running before release; private writes and Gateway |
| Storage | HDD RAM backing for baseline/pause; SSD for offload | Same; workspace, overlay and logs remain on HDD |
| Binary hash prefix | `cd7a875a7202` | `430fa7bde1e58`, control/logging fixes |

After about 20 ms of model waiting, management attempts idle handoff and pause. Offload may be attempted after about 50 ms, with at least one second of real-timer budget and a five-second cooldown. Response delivery, real timers, external events or fail-safe handling can resume the VM; a request may have multiple handoffs. Download/file-read triggers and streaming buffers are disabled.

The 16 services share Host CPUs 16–31 with a one-core quota each, not exclusive core binding. Host Agent/replay uses 0–7 and 10–15; BPF uses 14–15. Other evaluations on 8, 9, 40 and 41 are avoided, but shared disks and memory bandwidth remain potential interference sources.

Memory covers VM service cgroups, with guest RAM mapping PSS measured separately. Host Agent, replay server and BPF are excluded. Memory×time integrates occupancy over execution, rather than measuring peak or instantaneous use. Response-delivery latency is not the duration of the Resume command alone.

### Single VM: pause versus offload {#single-vm}

Baseline/pause below come from the later diagnostic reference, N=1 each. SSD offload is a separate three-run cohort: each cell is its own median, with minimum–maximum in parentheses. These are not three repeats of every condition or a randomized interleaved comparison.

| Metric | Management off | Pause only | SSD offload, median (range) |
| --- | ---: | ---: | ---: |
| Agent elapsed time (s) | 286.2 | 286.5 | 303.8 (299.1–304.1) |
| Guest RAM PSS×time (GiB·s) | 115.2 | 114.9 | 24.3 (21.5–25.9) |
| VM service memory×time (GiB·s) | 146.0 | 147.6 | 63.2 (62.3–97.4) |

Pause alone retains a similar memory integral to the unmanaged run. SSD offload records lower memory integrals and longer elapsed time, with substantial variation in service memory. Small samples, separate cohorts and different storage media prevent attributing percentage differences to a single mechanism or promising stable gains.

### 16 VMs: memory and batch completion {#concurrent-vms}

These October 22 batches follow the logging-blockage fix. There is one batch per strategy; all 48 replays pass functional checks. The Agent completion window spans common release to the last Agent completion, and throughput is 16 divided by that window in minutes. Separate subsequent verification is excluded. This is not average per-task latency.

| Metric | Management off | Pause only | SSD offload |
| --- | ---: | ---: | ---: |
| Agent batch completion window (s) | 381.50 | 443.23 | 447.36 |
| Agent throughput (tasks/min) | 2.52 | 2.17 | 2.15 |
| Mean total VM service memory (GiB) | 8.89 | 9.40 | 4.88 |
| Sampled peak total VM service memory (GiB) | 14.68 | 16.02 | 11.77 |
| Service memory×time per task (GiB·s) | 211.89 | 260.30 | 136.45 |
| Guest RAM PSS×time per task (GiB·s) | 162.68 | 171.67 | 57.04 |

The offload batch records lower mean memory and memory integrals, alongside a longer completion window and lower throughput. The 64 GiB limit was not binding, so maximum VM capacity was not measured: these results do not establish density or throughput improvements. HDD/SSD configuration differences are included in the observations.

**Functional success is separate from measurement integrity: the original `validation.json` reports `measurement_integrity_passed: false`.** Baseline/pause lost 36,185/51,169 BPF events; recorded maximum per-VM resource sampling gaps were 4.24/21.65 seconds, versus 0.85 seconds for offload. BPF cannot support complete causal attribution; sampled peaks and PSS integrals must be interpreted with the gaps. Service cgroup accounting and guest PSS are distinct metrics. These historical observations are retained for review, not as a performance baseline with completed measurement acceptance.

### Sources and further validation {#sources}

This is the registered SWE resident engineering comparison. See its [registry entry](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/REGISTRY.md#b-swe-resident-eng) and the [PR-version runner documentation](https://github.com/DeepLink-org/pvisor/blob/7b9110b7a66ec8e2b7b1b97414ea42fce3bb61ca/benchmark/pvisor/sweeval/README.md). Original files remain unchanged in local outputs archives and are not published with the website:

- `pvisor-swe-reload-diag-20261021/cohort/summary.json`: single-VM baseline/pause above; the initial `pvisor-swe-replay-20261021` cohort is separate and not pooled.
- `pvisor-swe-ssd-offload-20261021/cohort/receipt.json` and `summary.json`: three SSD runs.
- `pvisor-swe-concurrent-fixed-20261022/experiment-plan.json`, `summary.csv`, `summary.json` and `validation.json`: concurrent configuration, statistics and measurement limits.

Further validation needs sources for the preliminary three groups, repeated interleaved runs on matching storage, complete observations, and memory-constrained capacity tests. Download and lazy-image performance require separate experiments; model-wait replay cannot substitute for them.

# When does Linux memory compression help, and what does it cost?

## Conclusions

**The clean optimized-release single-VM preflight shows a real charged-memory advantage for stable compressible data, but not a free reduction in resource needs: cold-on has higher ready and peak memory, higher total CPU, and a first-access fault/recovery tax. Whole-VM offload does not yet have an eligible complete formal cohort from these runs.** These are engineering observations, not a recommendation to enable compression by default.

| Decision | Evidence-supported interpretation |
| --- | --- |
| Keep stable compressible RAM cold | First-window repeated-data cgroup memory was 316.098 MiB off versus 89.332 MiB on; complete recovery checks passed. |
| Budget startup and recovery headroom | Repeated-data peak was 320.297 MiB off versus 392.676 MiB on; first digest+device-I/O recovery was 150.045 versus 428.493 ms. |
| Expect the same gain after writes or for random payloads | Not supported. After a 100% random mutation, repeated-origin cold2 was 309.262 versus 279.023 MiB; random-origin cold2 was 234.094 versus 268.023 MiB. |
| Choose compressed whole-VM offload on formal performance evidence | Not established: one formal cohort was contaminated, the other stopped incomplete. |

This report stays in the engineering directory. B-COLD-RUNTIME-ENG supplies the cold-runtime observations; B-VM-MEMORY supplies separate SDK offload eligibility/preflight evidence. No public user benchmark numbers are updated. Historical GNU debug `lcr1` results in [the Linux cold-runtime report](LINUX_COLD_RUNTIME_REPORT.md) remain separate and are **not pooled** with release results.

## Motivation

An idle Agent VM can save memory only if encoded storage actually replaces charged resident pages, rather than merely adding a smaller copy. Choosing Linux compression therefore requires complete-group memory, recovery correctness, peak headroom and CPU costs together. Automatic live cold paging and explicit whole-VM offload have different service and timing boundaries; evidence for one cannot establish the other's benefit or concurrent density.

## Experiment design

### Controls, mechanisms and eligibility

- Clean cold-runtime cohort: Linux x86_64/KVM, host kernel `7.2.8-200.fc44.x86_64`, optimized dynamically linked GNU release (`x86_64-unknown-linux-gnu`, opt-level `z`, thin LTO, 8 codegen units, panic abort, stripped, `gateway` feature). It is neither debug nor musl. Retained harness prose saying “debug SDK timing” is stale for this supplied binary; hashing, device verification and diagnostic pause boundaries still apply.
- Four seeded shuffled off/on × repeated/random-unique conditions, **n=1 per cell**, each a fresh 256 MiB / 1-vCPU VM with a fully checked 64 MiB payload. Actual maximum simultaneous VM count is one, within the protocol's maximum four. Complete measured worker/VM/helper/store group: four CPU quota, 2 GiB memory limit, zero swap; inherited affinity permits CPUs 0–15, not dedicated cores.
- Sequence: ready → fixed 20-second cold1 → full restore1 → mutation → fixed 35-second cold2 → full restore2 → exit. **Mutation replaces 100% of the payload with deterministic random content in both initial patterns.** “Repeated” in cold2 identifies its origin, not still-compressible payload. There are no snapshot/offload operations in this cold experiment.
- Cold-on uses prefaulted anonymous RAM and an instance-local userfaultfd pager/store; off uses the default shared-file RAM path. Thus the switch changes backing, prefault and reclaim policy together: not an isolated codec-only causal A/B. On ordinary-RAM eligible VMA union is 272 MiB, exactly matched to complete smaps VMAs; off attribution uses the runner's backing inode. Configured 256 MiB guest RAM is not a proportional PSS estimator.
- Main metric is whole-group `memory.current`, including charged anon/file/kernel and helper/store/cache costs; lifecycle `memory.peak` includes scratch and transient allocations. CPU is final cumulative cgroup `usage_usec`, not codec-only CPU. Recovery milliseconds are guest acknowledgment `read_ms`: full SHA-256 plus 64 MiB device write/fsync/readback, not host RPC time or isolated fault latency.
- Each cold launch requires 30 quiet seconds, with a 180-second admission bound. Retained same-user VM/build guards detected no foreign jobs in the clean four cells. Inaccessible FDs and polling gaps prevent a claim of host-wide isolation. No global KSM/sysctl changes were made; administrator-enabled KSM is not evidence of RAM merging, and cold-on is incompatible with KSM advice. Userfaultfd device authority is administrator-granted.
- Reject failed integrity, lifecycle, budget, admission or interference checks; retain all failures, never score them as zero. No replacement cells or cross-cohort completion. With n=1 there are no causal percentages, confidence intervals, distribution estimates, P95/P99 or statistically established effect sizes. Neither these trials nor incomplete offload cohorts establish production density, business throughput, long-lived hot-set behavior, or superiority to Docker/other VMs.

SDK offload is a separate fresh-VM protocol: raw/compressed backing × repeated/random, 256 MiB VM and 64 MiB checked private data, two-core / 2 GiB / zero-swap service cgroup, CPU affinity 0,1, two-second active/parked settling, and 50 ms group memory monitoring. The guarded protocol observes interference from the coordinator outside the measured service, excludes only its UUID-owned cgroup descendants, rejects any detected foreign VM/build and stops the campaign. Its immutable/mutable-state, first complete scan, resume acknowledgment and heartbeat boundaries are not interchangeable with cold-runtime digest+I/O recovery.

### Recorded commands and reproduction

These are recorded commands, **not commands run while writing this report**. Build cwd was `/home/reiase/workspace/pvisor/benchmark/.data/mc1/src`, frozen dirty source; receipt environment included `CARGO_BUILD_JOBS=4`. The successful build command in `mc1/build-command.json` was:

```sh
cargo build --release --locked --offline --target x86_64-unknown-linux-gnu \
  --target-dir /home/reiase/workspace/pvisor/benchmark/.data/mc1/target \
  -j 4 -p pvisor --bin pvisor --example vm_live_memory_bench \
  --example vm_cold_runtime --features gateway -vv
```

Measurement cwd was `/home/reiase/workspace/worktrees/pvisor/stocky-osprey/pvisor`. The separate guarded preflight, stopped formal campaign and clean cold preflight are transcribed from `ml2-execution/commands.txt`:

```sh
/home/reiase/.pyenv/versions/3.12.11/bin/python3 benchmark/pvisor/live_vm_memory.py \
  --example /home/reiase/workspace/pvisor/benchmark/.data/mc1/bin/vm_live_memory_bench \
  --build-receipt /home/reiase/workspace/pvisor/benchmark/.data/mc1/build-receipt-vm_live_memory_bench.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/mlp2 \
  --samples 1 --warmups 0 --cpu-affinity 0,1 --budget-mib 2048

/home/reiase/.pyenv/versions/3.12.11/bin/python3 benchmark/pvisor/live_vm_memory.py \
  --example /home/reiase/workspace/pvisor/benchmark/.data/mc1/bin/vm_live_memory_bench \
  --build-receipt /home/reiase/workspace/pvisor/benchmark/.data/mc1/build-receipt-vm_live_memory_bench.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/mlf2 \
  --samples 30 --warmups 3 --cpu-affinity 0,1 --budget-mib 2048

/home/reiase/.pyenv/versions/3.12.11/bin/python3 benchmark/pvisor/linux_cold_runtime.py \
  --example /home/reiase/workspace/pvisor/benchmark/.data/mc1/bin/vm_cold_runtime \
  --build-receipt /home/reiase/workspace/pvisor/benchmark/.data/mc1/build-receipt-vm_cold_runtime.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/mcr2
```

`ml-execution/commands.txt` records the same interpreter, entrypoints, binary/receipt/rootfs/firmware paths and options with outputs `mlp`, `mlf`, `mcr`, respectively. Those are different cohorts with different frozen harnesses, not alternate samples of the guarded campaign. The exact command-file SHA-256 values are `5e2103c1e6cbb0d0b6576b83f17b56e36056f3e8e9efb2cb581ae25020ec3c2d` and `2e760062d3b83d7bbbbd46e596528262af85d51b9731cc6365c95949d94fc3c2`, respectively. Reproduction instructions are in [the cold-runtime runner subsection](README.md#cold-runtimestorage-validation) and [SDK offload subsection](README.md#linux-sdk-live-ram-offload). Use NEW short disk output paths, freeze receipts first, coordinate a quiet host, and never overwrite retained cohorts or build/test during sampling.

## Data and analysis

### Clean release cold-runtime levels

Cohort `mcr2` only; one observed value per fresh cell, **not P50/P95**. Memory in MiB (2²⁰ bytes), CPU in seconds, recovery in ms. Peak and CPU come from raw final `after.counters`; phase memory comes from `observations.phases`.

| Initial payload | Cold | Ready | Cold1 (20 s) | Cold2 (35 s, after random mutation) | Lifecycle peak | Total CPU | Restore1 digest+I/O | Restore2 digest+I/O |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| repeated | off | 314.699 | 316.098 | 309.262 | 320.297 | 11.141 | 150.045 | 383.890 |
| repeated | on | 391.148 | 89.332 | 279.023 | 392.676 | 12.040 | 428.493 | 188.810 |
| random-unique | off | 294.328 | 295.688 | 234.094 | 303.766 | 11.956 | 186.437 | 389.663 |
| random-unique | on | 372.293 | 262.555 | 268.023 | 377.387 | 12.256 | 283.278 | 190.133 |

| Initial payload | Observed on − off ready (MiB) | Cold1 (MiB) | Cold2 (MiB) | Peak (MiB) | CPU (s) | Restore1 (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| repeated | +76.449 | −226.766 | −30.238 | +72.379 | +0.899 | +278.448 |
| random-unique | +77.965 | −33.133 | +33.930 | +73.621 | +0.300 | +96.841 |

Differences use unrounded inputs and are **single-pair observations**, not causal percentage improvements or statistical estimates. Stable repeated contents show the clearest cold-memory advantage, while the elevated ready/peak levels show why cold occupancy cannot define a launch budget. First recovery includes a visible fault/recovery tax and total CPU is higher in both on cells; the smaller second-recovery values do not guarantee faster access. After writes, repeated-origin first-window savings do not persist at the same level, and random-origin cold2 has no relative charged-memory advantage. Off-mode file-cache reclamation also changes charged memory without invoking compression.

### RAM and store evidence: counters are not net savings

Cold-on only, n=1 per cell/window; MiB except rejection count. Gauges are latest live observations with the harness's five-second freshness bound. Totals are cumulative at that marker, **never summed across phases**.

| Initial payload | Window | Encoded store gauge | Cold-byte gauge | Discarded total | Restored total | Put rejections total |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| repeated | cold1 | 4.285 | 251.875 | 266.000 | 14.125 | 1 |
| repeated | cold2 | 4.142 | 105.750 | 284.375 | 178.625 | 1,971 |
| random-unique | cold1 | 4.317 | 124.312 | 137.250 | 12.938 | 2,117 |
| random-unique | cold2 | 4.282 | 122.812 | 157.375 | 34.562 | 4,170 |

Isolated RAM PSS ready→cold1 was 272.000→20.125 MiB for repeated/on and 265.750→147.688 MiB for random/on; off RAM PSS was 160.250→160.000 MiB in each cell. These are attributed RAM mappings, not the entire group or whole-host net memory. Encoded store bytes omit original RAM, metadata, indexes, allocator and scratch; small store gauges are **not** a net-memory result. Rejection totals are consistent with refusing unprofitable blocks, but random payload identity is not traced to individual host pages: other guest/OS pages can compress. Do not attribute all discard savings to random payload compression.

### Cohort eligibility and offload evidence

Raw roots below are literal local evidence directory names under `/home/reiase/workspace/pvisor/benchmark/.data/`, not public download links. No cohort is pooled with any other.

| Evidence root | Retained outcome | Permitted use |
| --- | --- | --- |
| `mc1` | Frozen dirty source, successful GNU release build, artifacts and test receipts | Provenance, not samples |
| `mlp` | SDK preflight: 4 native passes, n=1/cell | Separate preflight only; not formal performance |
| `mlf` | 132 native passes: 12 warmups and 120 formal rows; external VM overlap independently retained | **Not eligible** for a clean compression comparison despite native integrity passes |
| `mcr` | 4 native correctness passes, but 3 cells rejected for host interference | Failed/diagnostic cohort; not a cold off/on comparison |
| `mlp2` | Guarded SDK preflight: 4 accepted fresh cells, n=1/cell | Separate descriptive preflight only |
| `mlf2` | Stopped at 28 attempts: 12 passing warmups, 15 valid formal rows, 1 rejected formal attempt, 104 planned formal attempts unmeasured | **Invalid partial cohort**; no formal aggregate, causality, statistics or CI |
| `mcr2` | 4/4 accepted cold-runtime cells, no detected interference | Clean engineering preflight levels above |
| `ml-execution`, `ml2-execution` | Commands, independent inventories, outcomes, cleanup and summaries | Eligibility/receipt audit; not extra samples |

For `mlf`, `publication-eligibility.json` explicitly records external VM overlap through the independently retained `ml-execution/formal-host-inventory.jsonl`, interference summary and overlap-attempt inventory. Native correctness is necessary but does not establish timing eligibility; do not delete overlapping rows and publish the remainder. For `mlf2`, the guard detected foreign `/benchmark/.data/6e2/bin/pvisor` VM work and stopped; its retained formal counts are repeated/raw 4, repeated/compressed 4, random/compressed 4, random/raw 3, plus the rejected random/raw attempt. The 104 unmeasured cells are not failures or zero-valued samples. No partial confidence interval, pooled median, significance claim or completed formal compression result is reported.

Guarded SDK preflight `mlp2` alone, n=1/cell, observed levels; memory in MiB and times in ms:

| Payload | Backing | Active group | Parked group | Restored group | Offload | Resume acknowledgment | First restored scan |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| repeated | raw | 228.578 | 32.570 | 170.328 | 105.452 | 0.557 | 89.026 |
| repeated | compressed | 395.184 | 33.766 | 150.086 | 409.789 | 0.909 | 99.908 |
| random | raw | 242.699 | 34.855 | 184.703 | 102.800 | 0.617 | 108.303 |
| random | compressed | 396.215 | 34.430 | 221.137 | 575.047 | 0.418 | 154.140 |

Both backing modes parked near the same low charged-memory level in these isolated trials; compressed backing does not show a clear additional parked-memory benefit here, while its active level and offload time are higher. Do not mistake active→parked savings from whole-VM offload for the incremental benefit of compression, or substitute these preflight values for a complete formal experiment.

### Integrity and retained validation

Direct inspection of each `mcr2/attempts/<id>/raw.json` verifies all checks passed and five SHA-checked acknowledgments per cell (ready, restore1, mutation, restore2, exit): **20/20 total**, each checking all 64 MiB and reporting 64 MiB device I/O. Expected digests match repeated/random initial content and the fully mutated random state; heartbeat progresses, guest exits zero, all native children reap and all four owned units are quiescent. Each cell's 62 retained guard records contains zero detected foreign jobs. Integrity does not imply host-wide isolation or a statistical performance claim.

Retained source/build validation, not rerun for this documentation task:

- `CARGO_BUILD_JOBS=4 NEXTEST_TEST_THREADS=2 just test pvisor-vm`: 320 passed, 6 skipped.
- `just test pvisor` was not clean: an initial test compilation problem was repaired with the test-only `NetworkTransport::Tcp`→`TcpTunnel` fix in `crates/pvisor/src/runtime/run.rs`. The frozen source is dirty, not exactly the commit tree.
- Complete supplement `CARGO_BUILD_JOBS=4 NEXTEST_TEST_THREADS=2 cargo nextest run --locked -p pvisor --no-fail-fast`: 493 passed, 6 failed, 24 skipped. Retained failures concern unrelated local sandbox setup (`mount staged state at /home/reiase: Invalid argument`), across four `rootless_local` tests, one `run_config_cli` test and one `documentation_json` test. This is not a passing full pVisor suite; see `mc1/test-commands.json`, `test-pvisor-complete.log` and `handoff.json`.
- Release build receipt returns zero. No new build, code test, public-doc edit or approval-ledger edit was performed for this report; failed and partial raw evidence remains intact.

### Sources and derived download

Download [the clean release cold-runtime summary CSV](MEMORY_COMPRESSION_SUMMARY.csv). It contains only the four accepted engineering cells, retains configuration/n/statistic and raw/report/source identities, and is not a formal offload publication.

Actual source identity: **`d402d60b35804a0a4ca847c1960ac54ecb3a90c2` plus frozen dirty changes**, including the test-only repair and harness changes. Repository crates/vendor/Cargo files/`.cargo` were frozen; external registry source bytes were not. Supplied receipt/hash agreement is not an independent rebuild proof. Relevant SHA-256 identities:

| Artifact / receipt | SHA-256 |
| --- | --- |
| `mc1/source-manifest.json` | `c20eacb238f070e6e2e82bfc5efde565205498e4f3ad906e12d7872d983d44b2` |
| `mc1/bin/vm_cold_runtime` | `e3bf22d2ea9d33bc3c40b95cb6720b476a1d468fe6c0f433d26e03365b8649fc` |
| `mc1/bin/vm_live_memory_bench` | `e40db6a9373e37ebe10167307c32e66a2524d61617c02d386490cedf2e807431` |
| `mc1/build-receipt-vm_cold_runtime.json` | `bb9ce3b64ccc163e8d0f050390a15088f52ad4aa13ca122e72aeaccf85e2609c` |
| `mc1/build-receipt-vm_live_memory_bench.json` | `d32b368e36f99b1d7e44a1351f71900796a46259d7f352bcf022637438e6bdfc` |
| `mcr2/report.json` | `a6486be445b39dd15a7cb21f2b679e8b1da2f79178ce8c0f6dec11d9c01a02a5` |
| `mlp2/report.json` | `5ddb98fa6668acc8f48cccfdac4bec4fc7f4704fae0ad2ed2172c1ba6088c44a` |
| `mlf/report.json` (ineligible) | `224ee35ea0c04b24f5df1787854ca2eec9bf3c1e08d4d9807ae42ce9cf190937` |
| `mlf2/report.json` (partial) | `e7a32a39aec48f331b7a22b6cfe06cced265fc124e2a2f1ca269e2c4416d7958` |
| Cold harness `mcr2/harness/linux_cold_runtime.py` | `218dc3e2eeb3abfb680f09968d52221ff9698e7fe6ce98fd5c3c06431b34486c` |
| Guarded SDK harness `mlp2/harness/live_vm_memory.py` | `798dc696e501582d86bf29a4d1028aa276faffbbf29562547873c1ea5a300c48` |
| Firmware `libkrunfw.so.5.6.2` | `b61f68dac3ef20a88e1ee387733e4baed2f7c02edac940c8882e0b549dac95e4` |
| Cold rootfs manifest | `409dda7272809c772fd11a212b6aa80dca07a76e8ebf4baf26eaf6b9ee12c884` |
| SDK rootfs manifest (different enumeration) | `5fad6f94a076fdb2e8e157e9d57a52008f8a585de3c23f863798f2b4667484b6` |

Raw phase smaps/cgroup counters, guest checks, metrics, guards, stream/logs, binaries, source snapshots, manifests, failed cohorts and final-unit receipts remain in the nine local evidence roots listed above. They are not shipped as public assets. Further conclusions require a new complete, quiet, repeated cohort with unchanged controls; do not fill missing cells from these archives.

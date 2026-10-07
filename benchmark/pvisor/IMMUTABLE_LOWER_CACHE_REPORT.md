# Owned immutable lower cache: real host FUSE engineering experiment

## Conclusions

**On this Linux host, explicitly owned immutable physical-lower metadata caching reduced repeated real-FUSE metadata/open/read median operation time by 26.2% hot and 23.9% after kernel TTL expiry. Full contents and upper mutations remained correct.** This is a meaningful reduction, not an order-of-magnitude improvement or native-equivalent performance. The cache-on hot traversal still took 103.96 ms versus native 9.34 ms.

The same-fixture `git status + rg + full-byte verification` task improved 24.0% on a persistent mount; fresh-process/mount-through-unmount whole-task latency improved 19.6%. These are **B-FS-ENG engineering results**, not B-FS-TOOLS user data and not evidence of VM/OCI or complete Agent task acceleration. Independent **B-FS-DIAG** counters support reduced physical stat work, not an additive decomposition of wall time.

## Motivation

The engineering decision is whether a caller that really owns a lifetime-stable lower can profitably declare it immutable, rather than treating every lower as externally mutable. The declaration must not make an upper copy-up, rename, deletion, replacement or whiteout stale. Native and ordinary mutable FUSE controls distinguish physical metadata cache gains from transport and remaining overlay costs.

## Experiment design

### Conditions and ownership

All four conditions use one frozen release driver and identical generated inputs:

| Condition | Lower declaration | Cache control | View |
|---|---|---|---|
| native | owned fixture copy | not applicable | normal host filesystem |
| mutable | `lower_mutability = []` | enabled but ineligible | real FUSE |
| immutable-cache-off | `[LayerMutability::Immutable]` | `PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE=1` | real FUSE |
| immutable-cache-on | `[LayerMutability::Immutable]` | cache enabled | real FUSE |

The three overlay conditions share the **same generated physical lower**; each owns a separate upper, work directory and mountpoint. Native uses a byte-identical copy, never writes that shared physical lower, and only mutates its private copy in the final correctness probe. No external process is authorized by the harness to change the lower or uppers. This is caller ownership, not an inference from a read-only mount. Other lower aliases or hostile external mutation are outside the experiment's ownership assumption.

The driver uses `pvisor_overlayfs::api` and the `OverlayConfiguration`, `OverlayMounting` and `OverlaySessionControl` traits. `pvisor-overlayfs` CLI features are disabled. `default_permissions=true`, owner-only FUSE, and no permission workaround. Active `/proc/self/mountinfo` records prove real FUSE mounts with `default_permissions`. The existing kernel TTL (1 second), KEEP_CACHE behavior and observation implementation are unmodified. All conditions use **no preimage journal**, explicitly excluding preimage/review/apply cost. Cache success-only/physical-only behavior and the existing 4096-entry limit are implementation inputs, not alternative benchmark implementations.

### Inputs, workloads and timing boundaries

- 2048 unique regular files / **1,103,872 payload bytes**, 32 branches with 64 files each. Half the files are shallow; half pass through seven additional nested directories (`d16/a/b/c/e/f/g/h/...` through `d31/...`). Every payload encodes its relative path and a deterministic pattern.
- The fixture is a private committed Git repository. Automatic GC/maintenance is disabled in that repository; `GIT_OPTIONAL_LOCKS=0` prevents status index refresh. Generated physical atimes are initialized beyond the run window so ordinary relatime accesses do not change the promised lower metadata. This changes fixture preparation only, not host mount policies.
- Lower pre/post inventories include namespace, full SHA-256 bytes, inode/device/link count, modes, uid/gid, size, nanosecond atime/mtime/ctime and xattrs. Exact equality is mandatory.
- **hot:** untimed immediate same-view priming, then two complete passes, each issuing `symlink_metadata`, fresh open and full read; metadata size/type and all bytes verified. Shallow and deep files are combined in this operation, not independently attributed.
- **TTL-expiry:** the same priming, then 1.1-second wait **outside the timer**, then the same two passes. This expires the initial kernel attributes/entries; the second pass can be hot. This is not a fully cold disk or all-requests-expired workload.
- **readsearch:** one pass with fresh opens, full reads, full-byte equality and exact single-match verification, without an explicit metadata syscall in the workload. It still causes real FUSE open/read backing resolution. Its same-view priming is untimed.
- **tools:** clean `git status --porcelain --untracked-files=all`, `rg` exact matching path-set verification, followed by all-file byte verification. One complete host developer-tool task, not seven B-FS-TOOLS tasks or an Agent session.
- **whole-tools:** the same tools task on a fresh process/mount for each sample, timed by the coordinator from launch through successful normal unmount/process exit. Includes taskset/process/IPC overhead and mount teardown, excludes fixture copying/build/provenance/after-run mutation probes. Driver mount, tool-operation and unmount time are separately retained.
- Persistent mounts are repeated across the batch. At most one workload is active at a time, although three idle FUSE mounts remain alive. Conditions/workloads are seeded-shuffled each round, including fresh whole-task runs. Every cell has **3 retained warmups and 30 measured samples**; seed **4207**; CPU affinity **0,1** applies to driver, FUSE threads and tool descendants. No memory/cgroup quota or global cache drop is imposed.
- Formal timing does not enable `PVISOR_FS_PROFILE`. Profile is a separate cohort with **3 rounds / no warmups**, identical workload design and binary, and no profile timings in latency tables.
- Failures, invalid contents or lower changes fail the cohort; no slow valid sample is discarded. Median-ratio changes use **5000 paired-round bootstrap resamples**, 95% intervals. Existing `publication.distribution` checks separated clusters before presenting P50; all 20 cells were unsplit. P95 at n=30 is descriptive only; no P99.

### Correctness probes and limits

After sampling, each persistent view first warms successful lower resolutions, then appends to a lower file, verifies original-plus-appended bytes and new metadata length, renames it, unlinks/recreates a second file and removes a third lower-only file. The checks repeat after 1.1 seconds. ENOENT, recreated/renamed contents and merged directory names must be correct; `.wh.*` and removed names must not leak. Each overlay's physical upper must contain exactly the renamed file, recreated file, two required whiteouts and only the allowed root metadata bookkeeping file. Recreate must clear its old whiteout. The shared lower's full inventory must remain unchanged.

All checks passed. These are single-writer probes on warmed paths, not a concurrency, hardlink-alias, crash-recovery, permission-denial, eviction or all-namespace-operations proof. Writes are correctness probes **outside performance tables**, not evidence of faster writes. The deep fixture is exercised by every read traversal; the mutation probe uses three shallow paths. Physical cache occupancy did not trigger eviction in this input. Inputs are small, warm, read-heavy, and the host is one Ryzen/Btrfs machine.

## Data and analysis

### Formal uninstrumented timing

Host: **AMD Ryzen 7 9700X**, Linux **7.2.8-200.fc44.x86_64**, x86_64 GNU release; physical input/upper storage is Btrfs on `/dev/nvme0n1p3`, `relatime,compress=zstd:1`. Git and rg versions, complete CPU/kernel/mount inventory and allowed affinity are in the raw report. All figures below belong to `immutable-cache-timing-20261007` only.

**Milliseconds, P50 / descriptive P95, n=30 per cell.** Warmups excluded; no rejected/failed formal samples.

| Workload | Native | Mutable FUSE | Immutable cache-off | Immutable cache-on |
|---|---:|---:|---:|---:|
| hot metadata/open/read, two passes | 9.34 / 9.73 | 140.01 / 142.45 | 140.86 / 142.22 | **103.96 / 105.82** |
| TTL-expiry metadata/open/read, two passes | 13.56 / 13.89 | 150.56 / 157.21 | 147.71 / 155.34 | **112.47 / 120.42** |
| readsearch, one pass | 3.36 / 4.24 | 69.72 / 71.23 | 70.31 / 72.51 | **52.80 / 54.27** |
| tools, persistent view | 19.81 / 22.79 | 202.21 / 205.82 | 203.43 / 207.84 | **154.55 / 158.30** |
| whole-tools, fresh launch/mount/task/unmount | 22.66 / 24.75 | 210.76 / 214.81 | 210.49 / 214.17 | **169.28 / 174.48** |

Cache-on relative to immutable cache-off, ratio of medians; negative means less elapsed time:

| Workload | Change | Paired bootstrap 95% CI |
|---|---:|---:|
| hot | **−26.20%** | [−26.55%, −25.79%] |
| TTL-expiry | **−23.86%** | [−25.44%, −22.44%] |
| readsearch | **−24.90%** | [−25.48%, −24.22%] |
| tools | **−24.03%** | [−24.69%, −22.98%] |
| whole-tools | **−19.58%** | [−20.44%, −19.10%] |

All intervals exclude zero. Cache-off and mutable levels are close, as expected for the same owned inputs without reusable physical metadata; their small differences are not separately claimed significant. Native remains substantially faster. Residual work includes FUSE transport, uncached upper/whiteout/policy resolution, native backing opens and complete content handling. No exact additive wall-time attribution follows from this experiment.

### Mount and whole-task are different metrics

Driver **mount/unmount P50 in ms, n=30 fresh whole-tools samples**:

| Condition | Mount setup | Normal unmount |
|---|---:|---:|
| native (no mount; bookkeeping only) | 0.00042 | 0.00015 |
| mutable | 0.987 | 3.237 |
| immutable cache-off | 0.937 | 3.151 |
| immutable cache-on | 0.948 | 3.011 |

Mount time starts immediately before configuration/mount preparation and ends after the mount returns. It is not subprocess-start-to-ready time and not whole task time. The separately timed whole-task table includes launch/IPC and teardown as well as tools. Medians of these overlapping boundaries must not be added to manufacture a whole-task median. Persistent process lifetimes were about 261–264 seconds including all waits, other conditions' work and final probes; those are retained lifecycle records, **not operation latency**. The final correctness commands include an intentional 1.1-second sleep and are not write benchmarks.

### Independent diagnostic profile

`immutable-cache-profile-20261007`: **138 cumulative records, 24 PID/component/instance identities**, all have final records. There are three persistent overlay mounts and nine fresh whole-tools overlay mounts, each with an overlay-core and host-fuse instance. Native emits no FUSE profiles. Every record is retained byte-for-byte in regular-file stderr, also parsed in the raw report. The derived [instance counter inventory](immutable-lower-cache-counters.csv) includes **all 24 final instances**, not just a favorable root/leaf subset. A missing measurement has zero recorded units here; native has no such instance and is not a zero-work FUSE control.

Persistent **overlay-core final `measurements.*.units`**, each includes 3 rounds, untimed priming and final mutation probes:

| Condition | Physical parent stats | Physical leaf stats | Cache hits | Cache misses | Cache evictions |
|---|---:|---:|---:|---:|---:|
| mutable | 1,517,298 | 1,098,090 | 0 | 0 | 0 |
| immutable cache-off | 1,519,090 | 1,099,562 | 0 | 0 | 0 |
| immutable cache-on | **772,137** | **173,019** | **925,807** | 3,121 | 0 |

For that complete persistent instance, parent stats drop **49.2%**, leaf stats **84.3%** versus cache-off. Uncached mutable/upper paths and rejected/non-successful cache candidates still incur physical checks. Misses are not a count of distinct cached successful entries; the cache stores successes only.

Each of the **three independent fresh whole-tools overlay-core instances** has the following final counters (same values in all three rounds, not a sum):

| Condition | Parent stats / instance | Leaf stats / instance | Hits / instance | Misses / instance | Evictions / instance |
|---|---:|---:|---:|---:|---:|
| mutable | 98,154 | 71,417 | 0 | 0 | 0 |
| immutable cache-off | 98,154 | 71,417 | 0 | 0 | 0 |
| immutable cache-on | 59,881 | 13,682 | 57,735 | 2,512 | 0 |

Persistent host-fuse final request calls:

| Condition | lookup | getattr | open | read | readdir |
|---|---:|---:|---:|---:|---:|
| mutable | 22,893 | 59,882 | 86,081 | 86,083 | 1,760 |
| immutable cache-off | 27,277 | 55,788 | 86,081 | 86,083 | 1,760 |
| immutable cache-on | 25,085 | 57,835 | 86,081 | 86,083 | 1,760 |

Open/read/readdir counts remain equal. Lookup/getattr differ because unchanged kernel TTL interacts with elapsed execution and shuffled idle windows; this is not permission to attribute every request difference solely to the physical cache. Counters support reduced physical resolution work despite real repeated open/read requests. **Inclusive span durations are neither summed across spans nor added across overlapping instances**, and instrumentation elapsed times do not validate the uninstrumented speedup.

### Failures and validation

- An initial preparation preflight failed before mounting: Git commit's automatic background GC removed object directories during the inventory walk. The failed output remains in `benchmark/.data/immutable-cache-preflight-20261007/`; no timing was accepted from it. Fixture-local automatic GC/maintenance is disabled in the frozen measured harness.
- Two later independent n=1 preflight cohorts passed. They are not merged with the formal cohort. The final preflight includes append-preserving copy-up and merged-directory checks.
- Formal: **660 recorded workload results = 60 warmups + 600 measured samples**, all passed; four persistent mutation probes passed; exact shared lower pre/post inventory equality passed; binary/frozen-source/harness receipts passed before and after.
- Profile: all conditions, complete tasks, lower/upper checks and final-instance coverage passed independently. No real mount restriction occurred, no mock/core-only fallback was used and no safety policy was relaxed.
- Conventional validation: `python3 -m pytest benchmark/pvisor/test_immutable_lower_cache.py -q`: **6 passed**. The tests cover fixture shape, complete deterministic shuffled rounds, paired CI, cumulative-profile replacement/all-instance retention, explicit missing finals and inventory change detection. Release driver build and real-FUSE preflight/batch execution provide integration validation; no unrelated crate/workspace test was run.
- A subsequent harness audit requires exactly one final `overlay-core` and one final `host-fuse` instance for every expected overlay worker log, permitting no FUSE instances for native. Empty logs, missing components and absent expected logs fail explicitly. The strengthened checker revalidated all 24 retained diagnostic instances without changing the original evidence or timing cohort; conventional coverage now has 11 passing tests. A new one-round real-FUSE profile smoke passed with a fresh build/harness receipt in `benchmark/.data/immutable-cache-build-guard-20261007/` and `benchmark/.data/immutable-cache-profile-guard-smoke2-20261007/`. The prior attempt correctly refused the changed harness against the old receipt before mounting; that failure remains in `benchmark/.data/immutable-cache-profile-guard-smoke-20261007/`. None of these supplemental checks enters formal timing distributions.
- Post-run `/proc/self/mountinfo` contained **no experiment mountpoints**. All owned driver processes exited normally. Raw failed and successful stages are retained for inspection rather than deleted.

## Reproduction and retained evidence

See [the runner manual](README.md#owned-immutable-lower-physical-metadata-cache) for portable commands and boundaries. Actual successful build and batches, run from repository root:

```sh
python3 -m pytest benchmark/pvisor/test_immutable_lower_cache.py -q
python3 benchmark/pvisor/immutable_lower_cache.py --build \
  --output benchmark/.data/immutable-cache-build-20261007-v3
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-20261007-v3/build-receipt.json \
  --output benchmark/.data/immutable-cache-preflight-20261007-v3 \
  --samples 1 --warmups 0
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-20261007-v3/build-receipt.json \
  --output benchmark/.data/immutable-cache-timing-20261007 \
  --samples 30 --warmups 3 --seed 4207
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-20261007-v3/build-receipt.json \
  --output benchmark/.data/immutable-cache-profile-20261007 \
  --profiles --samples 3 --warmups 0 --seed 4207
```

Existing output directories are intentionally rejected; choose new names when reproducing. Default affinity resolved to `0,1` in these commands. Formal timing was explicitly announced after successful preflight; build, conventional tests and profiles did not run in parallel with that window.

The isolated driver manifest has `[workspace]`, frozen path dependencies and a frozen vendored fuser patch. Source inventory includes **1117 tracked/nonignored crate/vendor/config/manifest files**, preserving user dirty bytes without editing them. `rustc 1.98.1`, `cargo 1.98.1`; actual build command:

```sh
cargo build --offline --release \
  --manifest-path /home/reiase/workspace/pvisor/benchmark/.data/immutable-cache-build-20261007-v3/driver/Cargo.toml \
  --target-dir /home/reiase/workspace/pvisor/target -j 4
```

All raw output is ignored/local, under separate `benchmark/.data/immutable-cache-*` directories: source snapshots, full build/source receipts, generated manifest/lock, release binary, fixture/pre/post inventories, launch commands/env, seeded order, protocol logs, mountinfo, full profile stderr, samples and reports. Reuse of the main Cargo target was only a build optimization, outside sampling. No `crates/**` or `docs/**` file was edited.

Derived tables: [timing summary CSV](immutable-lower-cache-summary.csv), [all-instance diagnostic counters CSV](immutable-lower-cache-counters.csv). Their cohort IDs, sample counts and statistical boundaries remain explicit. Raw data is not published as a user-page asset.

SHA-256 provenance:

| Artifact | SHA-256 |
|---|---|
| Release driver | `e6a9da69a755af3694bc99c6aa032b878cd3b669a50652313b7e86a90260e589` |
| All-source inventory JSON | `bd9d6c61aac091f34bcc4adc6af60d62d137aea59d2588719ac6965532bcc15e` |
| Runner Python | `19dc9d526cae49f0813af8ce72fecb8e147bc9e0f34020e3d11b39338b7c62ec` |
| Driver Rust source | `ec62f915d7047f8d04d4835fb32ce48bac6b0e9b8ca747f89e459c8c2d764647` |
| Timing host inventory | `db56a4f544a1b31fe6ba197e33d40c633ef8b394439009d77ea1db11273f5d21` |
| Timing input inventory | `613d3c53b975892d2d32869830b2e3fb77da51d114fcb54500e0098deb78f5ef` |
| Profile input inventory | `e6328debca663b1e3b305e713dc3e7ccf63870964e19d429f71acb952c507954` |
| Timing raw report | `13f04fea16bc4443b3f4920e7152c80c24cc3d5eaf0f1e3bb4bf7ca52fb2c480` |
| Profile raw report | `74c2e397382e6e0efb2cac35e4cad9d64444951a62a75d2c5f15d38efba8e6a1` |
| Derived timing CSV | `5a2b1ff3427dfd0cbba70d808105a9cd6258e9a08c846001258b3dc7ce8fd825` |
| Derived counters CSV | `4690f21392e9bdb5eb622a7e136bac4d83b239a4a028920203ff44aef652ffa7` |

Host hash is SHA-256 of UTF-8 `json.dumps(report['host'], sort_keys=True)` with default separators. Input hashes cover exact physical inventories, so separate cohorts' inodes/times/Git commit identities intentionally differ; they are not pooled. Source and binary identities are shared. The full manifest/lock/compiler/tool versions and the derived hash mapping are retained in `build-receipt.json` and timing `derived-provenance.json`.

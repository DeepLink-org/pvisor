# Memory-constrained Agent density replay

Benchmark: [B-SWE-MEMORY-ENG](../REGISTRY.md#b-swe-memory-eng), engineering A/B.
The question is whether offload can finish identical offline Agent work with less
queueing and memory-time under the same scoped RAM budget. This harness replays
model responses and their original waits; commands still execute in real VMs.

This directory contains **benchmark code, not a runtime optimization**. It changes
neither the guest kernel nor pVisor's pause/offload/resume implementation. The
historical experiment is in [RESULTS.md](RESULTS.md). It was not rerun to prepare
this change. The registry measurement stays unprepared for current main.

## Review map

| File | Responsibility |
|---|---|
| `protocol.json` | Exact 60 arrivals, budgets, unchanged execution/fault bounds, admission rules and pinned binary |
| `admission.py` | FIFO new-VM memory admission: half clean-cache credit, boot reservation, 8 GiB headroom and PSI check |
| `arrival_batch.py` | Arrival/queue/drain loop, 2-second host sampling, spawned acceptance workers, owned cleanup |
| `worker.py`, `replay_reference.py` | Explicit frozen SWE bridge adapter, loopback response replay, exact commands/patch/result acceptance |
| `analyze_job.py`, `audit_shutdown.py`, `review.py` | Execution and control checks, seal/log-drain/summary timing, actual exit, final condition acceptance |
| `run_pair.py`, `verify_inputs.py` | Finite baseline/offload sequence, prepared-bundle checks and hash verification inside each budget |
| `report.py` | English table with relative changes and 2x2 memory/active/queue/completion plots from existing evidence |
| `test_*.py` | Admission, real loop with simulated time, real multiprocessing spawn, failure and statistics contracts |

`max_queue_s: null` retains every pending request. Admission happens **before VM
creation** and never gates an existing VM's timer, external wakeup or resume.
There is no concurrent-offload count limit. Queued requests do not count as
active Agents. The old finite-expiry helper is retained only for regression
coverage; this fixed protocol forbids a finite queue deadline.

The 2400-second worker, 4200-second batch and 90-minute coordinator guards remain
fault bounds. Hitting one means failure/incomplete work, not queue rejection or
successful drainage. No failed cohort is retried automatically. Only successful,
strictly verified, actually exited VMs may release their owned RAM backing;
logs and overlays remain. A failure stops new admissions and preserves evidence.

## Fixed envelope and interpretation

- Two guest vCPUs; 250% whole-VM-service CPU, including VMM overhead.
- One active condition: 31.75 GiB RAM / CPU 5975%; outer coordinator: 256 MiB /
  CPU 25%. Combined: 32 GiB / CPU 6000%; swap disabled in both.
- Same 60 tasks (20 each Vuls, NodeBB, qutebrowser), same approximately 2.056
  Agent/min arrival table over 30 minutes, same FIFO rules and original waits.
- Disabled management versus full 50 ms idle-round SSD offload with existing
  range protection/decay. Existing safety, cooldown, timer and wakeup behavior
  are unchanged. Additional idle/offload diagnosis stays off.
- SSD RAM backing, overlay and journal; image/log filesystem remains separate.
  Each condition has independent image inodes. Its private caches, VM and helper
  charges are inside the condition cgroup; the coordinator is added separately.
  Common OS/shared executable cache is outside the scoped budget. Do not label
  this as an entire physical server with only 32 GiB. There is no hard IOPS quota.
- Full correct completion is mandatory. Throughput is verified tasks divided by
  first-arrival-to-last-verification time, including queue/drain. Memory-time
  integrates condition + coordinator samples; it is not guest RAM PSS alone.
  The raw observation interval is retained, without extrapolating memory to zero.

A finite batch can drain while its queue grows during arrivals. Neither drainage
nor observed active peaks establish sustainable capacity. The historical pair
was sequential, without randomization or repeats. Relative changes in the review
are descriptive arithmetic, not causal estimates with confidence intervals.

## Local acceptance (no VMs, no network models)

From repository root, using Python 3.10 or newer:

```sh
python3 -m unittest discover -s benchmark/swe_memory -v
ruff check benchmark/swe_memory
ruff format --check benchmark/swe_memory
```

`mise run test-py` also includes this directory. Tests use simulated clocks and
mock VM launches for loop behavior; separate tests start real `spawn` processes.
They do not claim that a fresh Linux/KVM integration run was performed.

## Prepared inputs: explicit external dependency

Current main retired the original SWE bridge and resident example. This adapter
therefore requires a **separately supplied frozen runtime bundle**. It does not
import `benchmark/pvisor/sweeval` from the current checkout, download credentials,
create a new model connection, or silently switch to the current CLI. The binary
hash in `protocol.json` identifies the measured build, not main. The adapter
asserts exact source shapes when adding stage placement and phase markers;
incompatible bridge revisions fail closed. Its `stop()` timing insertion checks
that removing the markers restores the original control flow and bounds.

Preparation is deliberately separate from timed execution. Use an explicitly
qualified bundle, not live paths from an earlier completed cohort. For each arm:

1. Create new HDD evidence and SSD backing/stage directories; never reuse jobs,
   batches or units. Provision a new user systemd slice with `MemoryMax=34091302912`,
   `MemorySwapMax=0`, `CPUQuota=5975%`. The coordinator runs outside both condition
   slices, capped at `MemoryMax=268435456`, `MemorySwapMax=0`, `CPUQuota=25%`,
   `RuntimeMaxSec=90min`. No system-wide sysctl or cache-drop operation is needed.
2. Prepare independent rootfs copies **inside each corresponding condition
   slice**. Preserve modes, identities, xattrs, symlinks and internal hardlinks;
   do not share image inodes across arms or read/prime them outside their budget.
   Keep original image/log placement. Never mutate the source images.
3. Supply `manifest.json` (binary/source hashes, qualified source trials), the
   three replay `plans/*.json`, three `configs/TASK-POLICY.json` and their
   `*-prepared.json` rootfs descriptors, reference/oracle evidence, and
   `advance-progress.json` with strictly qualified reference/trace status.
   Configs must retain `cpus: 2`, matching `management`, null `host_http_proxy`,
   and the pinned binary. Prepared rootfs descriptors point to this arm's copies.
4. Write `spec.json` and `admission-plan.json` directly from the corresponding
   objects in `protocol.json`. Create empty `jobs/`, `batches/`, `logs/`.
5. Write a context as below. `image_files` enumerates all regular, non-symlink
   files in the three private roots. `image_identity_sha256` is the digest from
   `verify_inputs.image_identity(context)`: content hashes, relative layout,
   mode/uid/gid, xattrs, symlink targets and hardlink grouping must match across
   arms. Create `frozen.json` with `files: {absolute_path: sha256}` covering the
   context, protocol, harness Python files, configs/prepared descriptors/plans,
   private image files, binary/bridge source, firmware, task and reference/oracle
   evidence. Hash/warm images only inside the corresponding condition slice.
   The hash inventory is an input, never generated by the measured worker.
6. Provide both context paths to `run_pair.py`. It refuses incomplete inventory,
   shared inodes, changed rules and existing output. Runtime cgroup limits and
   placement are checked before hashing/warmup and again during execution.
   The two arms must use the same CPU masks, supervisor and SSD filesystem.

Example context schema (replace paths and image inventory, not protocol values):

```json
{
  "context_file": "/campaign/baseline/context.json",
  "policy": "baseline",
  "root": "/campaign/baseline",
  "ssd": "/ssd/campaign/baseline",
  "slice": "pvisor-membench-b.slice",
  "unit": "pvisor-membench-b.service",
  "supervisor": "pvisor-membench-coordinator.service",
  "vm_cpus": "2-15,18-31,34-47,50-63",
  "helper_cpus": "0,16,32,48",
  "image_roots": ["/campaign/baseline/images/vuls-666", "/campaign/baseline/images/nodebb-291", "/campaign/baseline/images/qutebrowser-667"],
  "image_files": ["/campaign/baseline/images/vuls-666/EXAMPLE"],
  "image_identity_sha256": "REPLACE_WITH_VERIFIED_IMAGE_IDENTITY"
}
```

`pair.json` contains `contexts: {baseline: CONTEXT_PATH, offload: CONTEXT_PATH}`
and `output: NEW_REPORT_DIRECTORY`. Start once, inside the capped coordinator:

```sh
python3 benchmark/swe_memory/run_pair.py /campaign/pair.json
```

This is not a one-command installer for the archived images/bridge. Preparing
that external bundle and performing a fresh integration run are prerequisites
for new measurements. No setup, image copy or VM command above was executed as
part of preparing this local PR diff. The host storage reserves remain the
measured ones (100 GiB SSD hard / 110 GiB admission, 500 GiB HDD, 48 GiB host RAM).

## Render an existing pair

Matplotlib is needed only for rendering; the runner/tests use the standard library.
Raw records and generated HTML/PNG/JSON belong under ignored `.data/`.

```sh
python3 benchmark/swe_memory/report.py \
  --results /evidence/pair-results.json \
  --baseline-samples /evidence/baseline-resources.jsonl \
  --offload-samples /evidence/offload-resources.jsonl \
  --output benchmark/swe_memory/.data/review
```

Use `--historical` only for the supplied 2026-10-26 pair. Rendering cross-checks
the sampled memory integral against final acceptance, rejects incomplete work
and emits source hashes. Publish only the reviewed Markdown and SVG, not raw
traces, model transcripts, private paths or machine credentials. The delivery
P99 row is omitted from the table as requested; the performance cost remains in
the report prose. No full-run average-total-memory metric is displayed.

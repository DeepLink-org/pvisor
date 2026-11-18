# Native paired image startup experiments

Benchmark: B-LAZY-STARTUP (`benchmark/REGISTRY.md#b-lazy-startup`).

Question: with the same pinned Ubuntu source, what are the prepared startup,
cold/hot client loading and same-executor staged file-loading costs, measured
through verified Ready, normal Completion and completed response bytes?

## Integration

`register(&mut Criterion) -> anyhow::Result<()>` reads `PVISOR_BENCH_ENTRY` and
dispatches these entries:

- `startup/image_compare.json`: prepared OCI/overlayfs and TurboOCI/OverlayBD
  containers versus a materialized-rootfs pVisor VM.
- `startup/image_lazy_compare.json`: the same three conditions with a fresh
  private client followed immediately by its hot client, each using fresh
  tasks/VMs and writable state.
- `startup/stage_lazy_compare.json`: the same host staged executor reading four
  fixed source files through host FUSE versus OverlayBD ext4. It does not
  execute an image program or claim host-executor OCI rootfs support.

The containing executable must call `worker_dispatch() -> Option<i32>` **before
CLI parsing or entry-default normalization** and exit on `Some(code)`.
Prepared/lazy VM preparation reexecutes the executable under private
user/mount/PID namespaces. This early hook runs its native persistent owner
instead of starting another benchmark. `register` also handles the hook as a
fallback for direct module runners.

## Inputs and ownership

| Environment variable | Meaning |
| --- | --- |
| `PVISOR_BENCH_OUTPUT` | New evidence leaf below an existing `.data` parent. Stage requires a short leaf directly inside the owned lab. Existing output is refused. |
| `PVISOR_BENCH_LAB` | Existing, deployed private OverlayBD lab with a passed functional smoke and frozen source/derived identities. |
| `PVISOR_BENCH_PREPARED_STORE` | Existing public-cache-API prepared store for the pinned linux/amd64 source. |
| `PVISOR_BENCH_BINARY_DIR` | Prebuilt `pvisor` and `pvisor-daemon`. Lazy VM and stage preparation also require `examples/cache_probe` beside them. Prepared/lazy default to `target/release`; stage requires an explicit directory. |
| `PVISOR_BENCH_FIRMWARE` | Prepared/lazy dynamic firmware; defaults to `BINARY_DIR/libkrunfw.so.5`. |
| `PVISOR_BENCH_CPUS` | Two application CPUs, default `12,13`. |
| `PVISOR_BENCH_SERVICE_CPUS` | Two disjoint service CPUs, default `14,15`. |
| `PVISOR_BENCH_RUNTIME_RLIB` | Stage-only prebuilt runtime rlib, used to link the benchmark owner helper outside measurement. |

The fixed source is
`sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794`.
Source manifest/config/layer identities, copied inputs, binaries, firmware and
runtime libraries are checked and retained. Preexisting artifacts are not
claimed to correspond to the current product source without a build receipt.

The lab is locked for the whole cohort. Cold reset verifies idle namespaces,
tasks, containers, leases, mounts and host device inventory before stopping
only that lab. It archives only its `containerd-root`, `cache` and `run`,
preserves registry/conversion inputs and host page cache, restores service
affinity, and proves fresh client state. An incomplete reset is retained for
manual recovery, not rolled back or merged with the retired state.

Containers have two-CPU quotas and 2 GiB limits; VMs have two vCPUs and 2 GiB
guest RAM. These are not equal whole-node budgets. Stage does not claim an
enclosing memory limit. Both image services are local: this is not WAN, cold
disk, concurrent throughput or a pure format/VMM comparison.

## Measurement and evidence

Each experiment performs a checked functional preflight and three correctness
warmups. Criterion then owns adaptive warmup, calibration, iteration selection,
estimation and baseline comparison. Each `iter_custom` operation executes a
fresh randomized paired block and returns **only the sum of its selected
measured intervals**. Preparation, reset, validation, cleanup and disk evidence
writes are outside those intervals.

Ready and Completion have independent fresh-operation Criterion streams.
Every invocation still retains both clocks. Stage additionally registers a
fresh mounted-stage Completion stream. No stream replays another stream's
durations, and no fixed-count historical campaign is used as one operation.

Raw operation records and callback ledgers include Criterion warmup,
calibration and measurement. Criterion's public `iter_custom` API does not
identify their phase boundaries. The ledgers therefore record callback ranges,
iteration counts and returned nanoseconds **without guessing a formal subset**.
Criterion's own output is the active statistical output.

The Criterion score is the selected interval summed across the complete
randomized block, not a per-arm estimate. The preserved per-arm/per-state
records support auditing, but **do not satisfy the paired publication gate**.
Do not derive a user-facing ranking, paired interval, ratio, split-distribution
claim or historical-table update from an inferred subset of these callbacks.
Authoritative paired analysis and publication remain separate work.

Native observers retain exact argv/environment overrides, raw stdout/stderr,
monotonic timestamps and normal exit status. The authenticated cache proxy
forwards original frames only after complete body/digest verification. Raw
requests, response frames and bodies are buffered until the measured process
finishes, then written to the private evidence directory. This includes
authentication material: keep the evidence directory private.

Cache content, metadata and total framed response bytes are separate. Registry
content/metadata counts use completed response-body records and actual 200/206
body lengths, excluding HTTP/TCP overhead. Registry evidence does not establish
a Ready transfer cutoff. Cold content must be nonzero; hot content must be
zero. Host page cache is never globally evicted.

VM trials validate the real Bundle/isolation, canary, read-only workspace and
upper. Stage validates original OCI file bytes, actual read-only FUSE/ext4
mounts, per-path observed bytes, Bundle/empty changeset, owner receipts and
mounted-stage subclocks. Foreign build/benchmark work and changed host
TCMU/block/UIO inventories invalidate the cohort. Valid slow operations are
never discarded.

Any failure aborts sampling and retains partial evidence. Cleanup targets only
this experiment's owned process groups, namespace owner, task IDs, leases,
mounts and devices. Killing a Docker client alone is never treated as proof
that its remote work stopped. Final audits recheck source/artifact identity,
normal teardown and retained clock/byte/command evidence; they do not calculate
statistics or authorize publication.

The tracked Python entry scripts reject direct execution before preparation
or service startup. Read-only archived analysis helpers remain importable;
the retired entry scripts are not a second CLI execution route. No frozen
historical batch or historical result is rewritten.

## Validation

The standalone Criterion crate uses its own Cargo test runner, rather than
pretending it is a root-workspace Cargo package:

```sh
cargo test --manifest-path benchmark/criterion/Cargo.toml
```

Unit tests exercise scheduling, marker/exit/timeout observation, frame
fragmentation, corrupt-body rejection, raw-byte retention, source identities,
stage receipts and semantic evidence failures. These checks are not real
KVM/FUSE/OverlayBD experiments. Full functional validation requires the
explicit prebuilt inputs and owned lab above; do not run a costly campaign
merely to test compilation.

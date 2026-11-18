# Storage experiments

Question: for the same authenticated OCI input, what are the complete storage,
prepared execution/read, memory and remote-supply costs of V2 versus Docker?
Read [the registry](../REGISTRY.md) and [shared entry guide](../README.md).

All seven entries use [Rust/Criterion](../criterion/src/storage/mod.rs).
Build the shared crate's `storage-worker` before execution. Private HTTPS
fixtures use the default `storage-tls` feature; static guest workers use
`--no-default-features`.

| Entry | Operation per iteration | Example metric |
| --- | --- | --- |
| `storage/lazy_storage.json` | Fresh singleton or combined store inventory | `pvisor.allocated`, `docker.inodes` |
| `storage/image_storage.json` | Same inventory protocol, engineering role | Same inventory metrics |
| `storage/lazy_runtime.json` | Prepared startup pair or complete-product memory pair | `pvisor.ready_ns`, `pvisor.four` |
| `storage/native_runtime.json` | Same runtime protocol, engineering role | Same runtime metrics |
| `storage/lazy_local_reads.json` | Local Python or two-pass mmap pair | `pvisor.first_ns`, `docker.ready_ns` |
| `storage/lazy_remote.json` | Paired cold then warm supply for both products | `pvisor-cold.requests`, `docker-cold.ready_ns` |
| `storage/image_format.json` | New-process storage-reader pair | `candidate.open_ns`, `baseline.scan_ns` |

## Configuration {#lazy-image-user-decisions}

Set `PVISOR_STORAGE_CONFIG` to an absolute JSON file:

```json
{
  "output": "/absolute/repo/benchmark/storage/.data/NEW_LOCAL_READS",
  "binary": "/absolute/repo/target/release/pvisor",
  "worker": "/absolute/repo/target/criterion/release/storage-worker",
  "inputs": "/absolute/verified-inputs.json",
  "workload": "mmap-scan",
  "metric": "pvisor.first_ns"
}
```

```sh
mise run benchmark -- storage/lazy_local_reads.json
```

`PVISOR_BENCH_OUTPUT` overrides `output` with a new `storage-evidence` child.
Either path requires a `.data` ancestor. Existing evidence cannot be reused.
Prepare artifacts, images, private Docker and delegated resource budgets before
admission. Do not start a measurement against a global daemon.

- Inventory `image` selects an input name or `combined`. Measure the actual
  combined store; do not sum singleton costs to invent shared storage.
- Runtime metric suffixes `ready_ns`/`exit_ns` select startup; `empty`/`one`/`four`
  select memory. Negative baseline-subtracted increments remain raw evidence
  and fail a metric that requires nonnegative values rather than being clamped.
- Local reads use `python` or `mmap-scan`. Phase metrics require mmap.
- Remote requires `daemon` and the [cache probe](../criterion/README.md),
  with workload `shell`, `python` or `mmap-scan`. The separately recorded
  cache preparation clock starts after staging verified OCI blobs. The probe
  resolves the manifest and reads config and layers from that local directory.
  Both cold and warm states execute even when only one metric is selected.
- Image format requires `baseline`, `candidate`, `baseline_runner`. Defaults:
  `files=10000`, `directories=1`, `backend=pool`, `payload=repeat`.
  Optional `cpu` must be available. The runners must expose the same ignored
  measurement test with source equivalence checked after rustfmt.

## Measurement and correctness

Criterion uses Flat sampling with 30 statistical samples and a three-second
warmup. Every iteration performs the complete randomized pair.
Nanosecond metrics use `iter_custom`; bytes, inodes and requests use their actual
units. Criterion receives the selected genuine observation, not pair wall time
or a cached value. All other arms, states and clocks remain in raw records.
Publication precedes repeated storage-reader timing and is reported separately.

OCI descriptors and configs must match SHA-256 and linux/amd64. Verify layers,
whiteouts, bytes, inventories and outputs, actual runtime/Ready/exit status,
resource limits and cleanup ownership. A failure invalidates the whole cohort.
Never discard a valid slow observation.

## Evidence and validation

Retain exact argv/environment, output, timeouts, source/product/tool hashes,
input blobs, inventories, pairing order, resource witnesses and verified network
frames. Criterion operation mappings include calibration/warmup without guessing
which callback was a formal sample. The evidence and Criterion output are
different artifacts; both are required.

Independent-arm summaries are not paired treatment-effect intervals, operation
tail latency or multimodal-cluster analysis. Follow registry gates before
publishing conclusions; inventories do not establish runtime latency.

```sh
mise run test -- pvisor-benchmarks -- \
  --manifest-path benchmark/criterion/Cargo.toml -E 'test(storage::)'
```

Unit checks do not establish Linux/KVM, Btrfs, private Docker, delegated cgroup
or authenticated remote-service acceptance.

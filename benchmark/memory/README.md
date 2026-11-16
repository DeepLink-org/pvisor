# Memory experiments

The four active engineering experiments use
[Rust/Criterion](../criterion/src/memory/mod.rs). Read [the registry](../REGISTRY.md)
and [shared entry guide](../README.md). Command duration is not a substitute for
phase memory accounting.

## Configuration

Set `PVISOR_MEMORY_CONFIG` to an absolute JSON file:

```json
{
  "experiment": "pool_compare",
  "inputs": "/short/frozen-inputs",
  "docker_inputs": "/short/frozen-docker-inputs",
  "output": "/short/NEW-batch",
  "metric": "random-shared/firecracker/pvisor-physical/idle-60/charged",
  "seed": 20261018,
  "preflight": true
}
```

Select the matching `memory/EXPERIMENT.json` through `mise run benchmark`.
`PVISOR_BENCH_OUTPUT` overrides `output`. Use a new real-disk directory with a
short absolute path (under 50 bytes). Build/freeze inputs before admission;
the observer rejects concurrent VM/build/test activity.

`preflight: true` performs one randomized correctness matrix, without a Criterion
distribution. Formal sampling requires a different fresh output and
`preflight: false`. Criterion uses Flat sampling, ten samples, 5,000 resamples
and at least a one-second warmup/measurement target. Each operation executes
the complete condition matrix, so an invocation can take much longer.

## Metrics

The custom measurement is signed bytes (`B`). Zero benefit and negative savings
remain valid. The selected observation is the reference-minus-candidate phase
footprint; allocator values are trim-minus-inspect extra host-byte drops.
Neither guest capacity nor whole-command duration represents this quantity.

| Experiment | Matrix | Selector |
| --- | --- | --- |
| `allocator_reclaim` | 64/256 MiB × inspect/trim | `64-phys-footprint-extra-drop`, `256-phys-footprint-extra-drop`, `64-resident-extra-drop`, `256-resident-extra-drop` |
| `scanner_compare` | three patterns × off/KSM/adaptive SDK groups | `PATTERN/REFERENCE/CANDIDATE/PHASE/charged` or `pss` |
| `pool_compare` | three patterns × seven product arms | `PATTERN/REFERENCE/CANDIDATE/PHASE/charged` or `pss` |
| `library_compare` | seven shared-random library variants | `random-shared/REFERENCE/CANDIDATE/PHASE/charged` or `pss` |

Selectors have five slash-separated fields. Pool/library phases include
`idle-0/1/2/5/10/20/40/60`, `readback`, `cow-25`, `cow-100` and
`post-write-1/2/5/10/20/30`; choose one complete phase name.
Scanner uses `ready`, `cow25`, `cow100`. Reference and candidate must differ;
configuration is validated before VM launch.

## Prepared inputs

`pvisor-memory-prepare` owns Rust preparation; `pool`, `scanner`, `allocator`
and `docker` subcommands freeze sources, commands, tool identities and inputs.
Preparation never runs Criterion or grants permission to measure.

```sh
cargo build --locked --manifest-path benchmark/criterion/Cargo.toml \
  --target-dir target/criterion --bin pvisor-memory-prepare
target/criterion/debug/pvisor-memory-prepare pool --output /short/NEW-inputs \
  --firmware /absolute/libkrunfw.so.5 --kernel /absolute/fc-vmlinux \
  --library-rootfs /absolute/common-glibc-userspace
target/criterion/debug/pvisor-memory-prepare docker --inputs /short/NEW-inputs \
  --output /short/NEW-docker-inputs
```

The native library worker uses `dlopen`, STATX DAX and full-byte/SHA-256 checks.
Require `worker_protocol: "native-library-v1"` and a loader/libc-compatible worker.
Scanner inputs include its SDK probe, pool daemon, rootfs, firmware, source
archive and matching build receipt. Allocator inputs include `vmm`, firmware,
payload, reclaim SO and private root. Its receipt must bind every supplied
artifact, actual compiler/commands and source archive. Set
`PVISOR_GUEST_RECLAIM=1`; physical-footprint observation requires Apple Silicon.

Build `pvisor-memory-worker` and `pvisor-memory-guest`; product-linked VM owners
use `vm-probes`. `PVISOR_MEMORY_WORKER` may select a built worker explicitly,
otherwise it must be adjacent to the Criterion executable.

## Evidence and validation

Every operation retains complete randomized order, phase snapshots, full curves,
placement/resource checks, integrity oracles, argv, logs and cleanup evidence.
Worker accounting is inside the owned service; observers and audits are outside.
Integrity, input, budget, interference, timeout or cleanup failure invalidates
the cohort. Never terminate unrelated processes or alter global kernel policies.
Criterion outputs remain separate from operation records and phase highwater
values. Follow registry publication rules before interpreting any confidence
interval or combining cohorts.

```sh
mise run test -- pvisor-benchmarks -- \
  --manifest-path benchmark/criterion/Cargo.toml -E 'test(memory::)'
```

Offline tests do not establish native HVF/KVM, KSM, private Docker, delegated
cgroup, DAX/PTE or whole-VM accounting coverage.

## Retained evidence readers

`plot_pool.py` and `analyze_pool.py` read saved curves through `pool_evidence.py`,
`scanner_evidence.py` and `tree_evidence.py`. Those modules contain no sampling,
process launch or preparation loops. Exact schema names describe saved records,
not selectable engines. `library_protocol.py`, `library_evidence.py` and
`docker_support.py` retain the independent page-identity, container ownership and
userspace verification used by these readers; they are not called by Criterion.
New input preparation is `pvisor-memory-prepare`, not a Python coordinator.

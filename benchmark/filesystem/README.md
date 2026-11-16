# Filesystem experiments

Read [REGISTRY.md](../REGISTRY.md) and the [benchmark entry guide](../README.md).
The three active entries use [Rust/Criterion](../criterion/src/filesystem/mod.rs).
They answer different questions and do not produce a combined performance score.

| Entry | Question | Configuration |
| --- | --- | --- |
| `filesystem/tools.md` | How much time do complete native metadata/read operations take? | `PVISOR_BENCH_FS_WORKER` |
| `filesystem/user_workloads.json` | How do mechanisms, resource limits and complete tasks compare across nine runtimes? | `PVISOR_BENCH_LAYERED_CONFIG` |
| `filesystem/dax_ab.json` | How does DAX change verified immutable-share reads? | DAX artifacts and `PVISOR_BENCH_METRIC` |

```sh
mise run benchmark -- filesystem/tools \
  --output benchmark/filesystem/.data/NEW-native
```

## Native probes

Build `filesystem-worker` in the Criterion crate. Set `PVISOR_BENCH_FS_WORKER`
to its absolute path. The worker uses 32 directories, 2,048 files and a 64 MiB
data file. Full fixture checks are outside the measured invocation; process
creation, workload/checksum and successful exit are inside it.
Child groups have a 30-second deadline. Every calibration and warmup invocation
has the same validation and raw logs as measured operations.

## Layered user comparison {#user-workloads}

Set `PVISOR_BENCH_LAYERED_CONFIG` to an absolute JSON file:

```json
{
  "rootfs": "/prepared/toolroot",
  "artifacts": "/frozen/cli-build",
  "firmware": "/frozen/libkrunfw.so",
  "firmware_source": "/frozen/SOURCE.json",
  "docker_setup": "/private/reference-budget/setup.json",
  "stock_kernel_receipt": "/official/stock/kernel-receipt.json"
}
```

The CLI directory must contain `build-receipt.json` and `bin/{pvisor,pvisor-guest}`.
Preparation builds static Rust payloads and reference PID1, creates deterministic
fixtures, freezes dependencies and prepares private disks/images. The Python,
git and C tools inside the workload remain real application subjects, not timers.
Requirements include Linux/KVM, user/mount/PID namespaces, musl, filesystem tools,
Firecracker, QEMU and prepared firmware/rootfs inputs.

Each operation randomizes all 19 cases across all nine modes. Select the returned
clock using `PVISOR_BENCH_CASE` (default `S01`) and `PVISOR_BENCH_BACKEND`
(default `vm`). Each environment has three correlated resident rounds.
Ready, setup, work, final sync, validation and exit remain separate. The selected
work+sync interval is passed to Criterion; all observations remain in raw records.
An invalid cell fails the complete operation, not a shortened matrix.

The parent budget is two CPUs, 2 GiB and zero swap. Private Docker must already
be provisioned with a verified setup receipt; the harness never adopts a global
daemon. Only owned containers and validated disposable outputs are removed.
Every cell has a 90-second deadline and an independent host final-content oracle.
`PVISOR_BENCH_LAYERED_INPUT` may select an already prepared bundle; it copies
verified inputs, never recorded measurements, into a fresh output.

## Immutable-share DAX

Set `PVISOR_BENCH_DAX_DRIVER`, `PVISOR_BENCH_DAX_GUEST`,
`PVISOR_BENCH_ROOTFS` and `PVISOR_BENCH_FIRMWARE` to prepared artifacts.
The guest and driver belong to the Criterion crate's `vm-probes` build.
`PVISOR_BENCH_METRIC` selects one of ten phase clocks (default `read_first`).
Each stream executes the full off/on pair but returns only the selected arm's
measured phase. The guest verifies statx DAX state and full byte content.
Read/parallel repeat intervals cover four passes; mmap covers sixteen.

The parent requires affinity 0,1, two CPUs, 2 GiB, zero swap and NOFILE 65536.
Input changes, OOM, interference, exit or validation failures invalidate the
cohort. Retain all valid slow observations.

## Evidence and validation

Use fresh `.data` outputs and freeze source, artifacts, inputs, argv, logs and
cleanup status. Criterion owns calibration, sampling, estimates and baselines.
Its batch estimates do not establish operation-level tail latency or paired
treatment-effect intervals. Publication follows the registry's additional gates.

```sh
mise run test -- pvisor-benchmarks -- \
  --manifest-path benchmark/criterion/Cargo.toml -E 'test(filesystem::)'
```

Offline tests are not KVM, DAX, cgroup, Docker or mount acceptance.
Experiment designs and recorded results remain in [WORKLOADS.md](WORKLOADS.md),
[CURVES.md](CURVES.md) and the relevant engineering reports. They are not
alternative measurement entry points.

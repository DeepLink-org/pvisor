# Startup experiments

Question: with prepared inputs, how long does an environment take to produce
verified Ready output and exit normally? Read [B-STARTUP](../REGISTRY.md#b-startup)
and the [benchmark entry guide](../README.md).

## Prepared-environment measurements

The [Rust implementation](../criterion/src/startup/mod.rs) supports native,
pVisor host/staged/VM, Docker, Firecracker, QEMU q35 and QEMU microvm:

```sh
mise run benchmark -- startup/native_timing \
  --output benchmark/startup/.data/NEW-native
```

`startup/timing.json` selects all eight conditions. Individual registered
`*_timing.json` entries select one condition.

| Variable | Input |
| --- | --- |
| `PVISOR_BENCH_BINARY` | Prebuilt pVisor binary |
| `PVISOR_BENCH_CPUS` | Two distinct available CPUs; defaults to the first two allowed CPUs |
| `PVISOR_BENCHMARK_ASSETS` | Frozen assets with `input-manifest.json`, for non-native conditions |
| `PVISOR_BENCHMARK_DOCKER_IMAGE` | Optional immutable local image ID; otherwise from `assets.json` |
| `PVISOR_BENCHMARK_KERNEL_RECEIPT` | Stock kernel receipt for Firecracker/QEMU, including QEMU initrd |

Never pull images or build subjects during sampling. VM and staged conditions
need KVM/FUSE, matching firmware, prepared userspace and the required permissions.
A live Host listener must match the selected executable digest.

An untimed preflight and three correctness warmups precede Criterion's adaptive
calibration. Each operation is a fresh execution. Ready stops at the complete,
exact `REFERENCE_READY` line; Completion requires normal zero exit.
Both clocks start immediately before spawn, and raw records retain both.
Criterion's streams perform separate fresh operations, not reuse cached values.
Input hashing, preparation, validation, logging and cleanup are outside the
returned durations. A controlled VM stop is not normal Completion.

Every invocation validates output ordering, input identities, canary and actual
isolation/Run Bundle where applicable. Pipes are drained concurrently with
bounded output and deadlines. Failure retains evidence and cleans owned process
groups. Disposable disks are removed only after successful validation; Docker
cleanup requires exact cohort ownership labels.

## Image and same-stage comparisons {#stage-lazy}

Select `startup/image_compare.json`, `startup/image_lazy_compare.json` or
`startup/stage_lazy_compare.json`. These require a dedicated private
[OverlayBD lab](OVERLAYBD_LAB.md), not an arbitrary existing Docker daemon.

- `PVISOR_BENCH_LAB`: owned `.data` lab directory.
- `PVISOR_BENCH_PREPARED_STORE`: verified immutable cache store.
- `PVISOR_BENCH_BINARY_DIR`: matching prepared `pvisor` and `pvisor-daemon`.
- `PVISOR_BENCH_CPUS` and `PVISOR_BENCH_SERVICE_CPUS`: disjoint available sets,
  defaulting to `12,13` and `14,15`.
- `PVISOR_BENCH_FIRMWARE`: dynamic firmware; default `libkrunfw.so.5` beside binaries.
- `PVISOR_BENCH_RUNTIME_RLIB`: release runtime rlib for same-stage measurements.

The lab requires Linux x86_64 namespaces, pivot-root permission, KVM and matching
dynamic tools/libraries. Same-stage preparation links the Rust owner outside the
clock and records compiler, rlib and command identities.
Both arms read and verify the same four OCI files with the same host probe.
Require real readonly mounts, successful rootless-process Bundle, exact bytes,
empty changeset/upper, cold nonzero/hot zero content and restored object inventory.

Each operation is a randomized paired block. A provider's cold client is
immediately followed by its hot client with fresh execution/mount/stage state.
Source and host page caches are retained; private reset is outside the client
interval. Ready, Completion, mounted-only Completion and transfer bytes are
separate quantities. Raw authentication-bearing evidence uses mode `0700`.

## Evidence and validation

Criterion owns sampling and statistics. Keep raw per-operation clocks, pipe
events, pairing order, source/artifact identities and cleanup results.
Do not infer operation P95/P99 from batch averages, combine incompatible clocks
or interpret independent-arm intervals as paired treatment effects.
Formal publication remains subject to [registry rules](../REGISTRY.md).

```sh
mise run test -- pvisor-benchmarks -- \
  --manifest-path benchmark/criterion/Cargo.toml -E 'test(startup::)'
```

Fixtures do not establish real KVM, FUSE, Docker, stock-kernel or lab capability.
Diagnostic protocols remain in [TURBO_OCI.md](TURBO_OCI.md) and
[STARTUP_EXPERIMENTS.md](STARTUP_EXPERIMENTS.md); they do not fill unprepared entries.

## Linux and Rust firmware stage diagnostic (KVM)

Build one release probe with the Rust kernel embedded. Supply the Linux firmware
for a GNU build and a trusted, disposable userspace with a working `/bin/sh`:

```sh
bundle=$(mise run kernel -- build --offline)
PVISOR_BUILD_NATIVE_KERNEL_BUNDLE="$bundle" mise exec -- cargo build \
  --release -p pvisor-vm --example vm_api_case
python3 benchmark/startup/firmware_stages.py --probe target/release/examples/vm_api_case \
  --linux-firmware /prepared/libkrunfw.so --rootfs /prepared/rootfs \
  --output benchmark/startup/.data/NEW-firmware
```

Both arms use 1 vCPU, 256 MiB and a fresh rootfs copy. The host interval starts at
`vm.runtime_configure` and stops when the guest acknowledges the virtio-MMIO console,
not when the console driver or shell is ready. It excludes firmware selection
before VM configuration. The shell must separately write the exact marker into
the shared rootfs and exit normally. `shell_file_median_ms` measures spawn to
host observation of those bytes (1 ms polling plus scheduling and filesystem
visibility); this is not a captured-stdio latency test.
Missing stages, failed shells and timeouts remain in the output; any incomplete
trial fails the command. `diagnostic.json` and the public Ready table are unchanged.

## Daemon VM warm pool {#daemon-vm-warm-pool}

[B-DAEMON-WARM-POOL](../REGISTRY.md#b-daemon-warm-pool) compares the same
optimized native daemon with and without one 4 vCPU / 256 MiB parked slot.
Prepare an Ubuntu 24.04 native OpenSandbox template with the bundled or
executable-adjacent Linux firmware; do not pass a template firmware override.
Prepare the inputs and compile the harness before starting the delegated scope:

```sh
/prepared/pvisor-daemon prepare-image --name ubuntu --image ubuntu:24.04 \
  --images-dir /prepared/images
cargo build --locked --release --manifest-path benchmark/criterion/Cargo.toml \
  --bin pvisor-benchmarks
systemd-run --user --scope --unit="pvisor-warm-$$" --quiet \
  -p Delegate=yes -p CPUQuota=400% -p MemoryMax=2G -p MemorySwapMax=0 \
  env PVISOR_BENCH_DAEMON_IMAGES=/prepared/images PVISOR_BENCH_DAEMON_IMAGE=ubuntu \
  taskset -c 4,5,6,7 benchmark/criterion/target/release/pvisor-benchmarks \
  startup/daemon_warm_pool.json --binary /prepared/pvisor-daemon \
  --output benchmark/startup/.data/NEW-warm-pool \
  --criterion-output benchmark/startup/.data/NEW-criterion
```

Choose four available logical CPUs. Use an otherwise idle KVM host and do not
run unrelated builds or tests on those CPUs. The scope includes both daemons,
VMs, brokers and observer. It must be fresh; the harness creates and cleans only
its own service and instance subtrees. It freezes the executable, Linux
firmware when external, template manifest and harness for the cohort.

The client times loopback HTTP from create to `Running` and from create to the
completed first shell command. Proof of a hit requires consuming the observed
parked slot, not just enabling pooling. Each burst starts with one slot and
checks two simultaneous matching creates, one hit and one pool-empty fallback.
The measured requests verify the image, command success, private data, actual
cgroup limits and cleanup. The test retains successful and failed evidence.

`pairs.jsonl` stores the individual observations. `summary.json` uses all
valid Criterion operations after the separate preflight and correctness warmups,
including Criterion's adaptive warmup. Its request P50/P95 and paired bootstrap
intervals are computed from the raw observations, not Criterion's batch
averages. The Criterion duration measures the serial pair's summed first-command
times; the burst is analyzed separately. The result is not a production hit
rate or a comparison against other runtimes.

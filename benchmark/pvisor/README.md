# pVisor benchmark

**Measures the process-level cost of a minimal host Run and of reading its
durable Run Bundle through `status --json` and `status --review --json`.**

Owns the smoke / nightly suites and the `pvisor-benchmark/v1` report schema.
Does not own Run lifecycle or isolation backends. Every sampled Run must finish
successfully and produce a completed, zero-exit-code bundle; a fast but
incomplete Run is rejected.

## Run

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly

just benchmark-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

`just benchmark-compare` takes candidate then optional baseline. The
`smoke` suite uses 2 warmups and 10 samples for pull-request feedback. The
`nightly` suite uses 10 warmups and 50 samples for a more stable distribution.
Both write the same `pvisor-benchmark/v1` raw schema and Markdown report.

Baseline and candidate measurements are meaningful only when collected on the
same host with the same suite. Comparison marks a metric as a regression when
it crosses the configurable threshold (15% by default); the report is
informational unless the runner is explicitly given `--fail-on-regression`.

Unit-test the report contract without running the suite:

```bash
just test-benchmark
```

## Sandbox startup and resource occupancy

`startup.py` is a separate Linux benchmark for the three executor levels and
host option costs. It leaves the existing `pvisor-benchmark/v1` smoke/nightly
report unchanged. The one-command entry builds a release binary, assembles a
small local rootfs with `/bin/sh`, `sleep`, `mount`, `env`, `rm`, and their
dynamic libraries, checks
each case, runs every available case, then writes JSON and Markdown reports.
It does not download an image or include build/setup time in the measurements.

```bash
just benchmark-startup
```

The results are `target/pvisor-benchmark/startup/startup.json` (raw samples)
and `target/pvisor-benchmark/startup/startup.md` (summary and skipped cases).
For a quick check, use `just benchmark-startup --no-build --warmups 0 --samples 1`.
`--adapter /path/to/adapters.json` includes third-party cases in the same
run. The entry reports missing `crun`/`runc`, KVM, or failed preflights as
skipped cases while continuing with available cases. It always runs `direct`
as a control. Failed preflight stderr is copied to `preflight-logs/` beside
the report.

To use a prepared image filesystem or control the comparison inputs, supply
explicit paths. The one-command entry copies those rootfs directories into
temporary setup storage to leave the originals untouched, then builds and
preflights before
sampling:

```bash
just benchmark-startup \
  --container-rootfs /absolute/path/to/rootfs \
  --container-pvisor-binary /absolute/path/to/linux/pvisor \
  --vm-rootfs /absolute/path/to/vm-rootfs
```

Use `just benchmark-startup-raw` to run `startup.py` directly with explicit
rootfs or image arguments and without automatic preflight. A pinned image
digest or immutable prepared rootfs is preferable for repeated comparisons;
keep any image download outside measured trials.

The default matrix includes `direct`, host without extra options, host with an
explicit stage, host `--safe` with the same explicit stage, cooperative network
proxy, deny-all network, safe plus deny-all, individual memory/process/file
descriptor limits, Gateway capture, local event recording, OCI container, and
libkrun VM. A case fails the run if the command fails or pVisor does not
produce a completed, zero-exit Run Bundle.
Container and VM cases also check the observed isolation in that bundle;
`--safe` host cases require `rootless_process`. The ordinary host case can
fall back to `host_process` on a system without the rootless sandbox: inspect
`observed_isolation` before comparing it with another machine. The VM case
sets `--overlaynet off` and the container case uses `--container-network none`,
so both run the payload without network access. The host `--safe`
preset changes filesystem and network configuration together; read its row as
the cost of that preset rather than the cost of one kernel mechanism.

For an initial capability check or a focused experiment:

```bash
python3 benchmark/pvisor/startup.py --output target/pvisor-benchmark/probe \
  --cases direct,host,host_stage,host_safe,container,vm \
  --container-rootfs /absolute/path/to/rootfs --vm-rootfs host \
  --warmups 0 --samples 1
```

`--vm-rootfs host` is Linux-only and exposes the host root to the guest as a
read-only base. Use a prepared VM rootfs for cross-implementation comparisons.
The low-level `startup.py` requires explicit rootfs inputs for container and
VM; the one-command entry prepares them locally. Trials use a fresh system
temporary directory, separate
from the report output. A failed trial and its stderr log remain at the path
printed by the script; `--keep-trials` retains successful trials too.
`--scratch-root` selects another parent directory when the same storage medium
must be used for all competitors. Avoid placing it inside the tested workspace
or pVisor's staged view.

Each randomized round runs two commands per case. `/bin/sh -c ':'` measures
**Popen to zero-exit completion** in milliseconds (p50, p95, mean). It includes
CLI parsing, sandbox setup, minimal command startup, Run persistence and
teardown. This is a cold-start *proxy*, not a timestamp for first guest
instruction. A separate `/bin/sh -c 'sleep 0.2'` run holds the sandbox open
for 200 ms while the sampler records peak live process count and the sum of
resident bytes in the launcher process tree. `--resource-hold-ms` changes that
duration; guests must support fractional `sleep`. CPU time is the
`RUSAGE_CHILDREN` user plus system delta for the minimal startup run. The
`--shell` flag changes the shell path in both workloads; use the same shell
build in each rootfs when comparing small startup differences. The
process tree is sampled every 2 ms by default. Short peaks and daemonized
descendants can be missed; summed RSS also counts shared pages more than once.
CPU accounting depends on the runtime waiting for its descendants.
For precise all-process memory or CPU attribution, collect cgroup v2
`memory.peak` and `cpu.stat` around the same commands on a dedicated host.

The JSON report preserves each measured trial and its observed isolation;
the Markdown table gives p50/p95 and ratios to direct execution. A second table
shows the median **paired** latency, CPU, and sampled RSS differences within
the same round. `host_safe` uses `host_stage` as its reference;
`host_safe_net_deny_all` uses `host_safe`; other host options use plain `host`.
Defaults are three warmups and 30 measured rounds with a fixed
randomization seed. The report records the kernel, CPU, memory, pVisor binary
hash, source commit, rootfs references, commands and sampling protocol.

### Comparing other implementations

Add argv templates through an adapter JSON file. `{workload}` expands to the
same three arguments used for pVisor. Other placeholders are `{workspace}`,
`{stage}`, `{record}` and `{run_home}`; each trial gets fresh directories.
`{port}` and `{admin_port}` allocate loopback ports before timing for services
that require an explicit port. There is a small bind/close race, so a port
collision fails that trial instead of entering the statistics.
For example, with the **same pinned OCI image** used by pVisor:

```json
{
  "schema": "pvisor-startup-adapter/v1",
  "cases": [
    {
      "name": "docker_container",
      "command": ["docker", "run", "--rm", "--pull", "never", "--network", "none", "--memory", "512m", "--cpus", "1", "my-image@sha256:REPLACE_ME", "{workload}"]
    }
  ]
}
```

```bash
python3 benchmark/pvisor/startup.py --output target/pvisor-benchmark/compare \
  --adapter /path/to/adapters.json \
  --cases direct,host,container,vm,docker_container \
  --common-memory 512MiB --common-cpu 1 \
  --container-image my-image@sha256:REPLACE_ME \
  --vm-rootfs image=my-image@sha256:REPLACE_ME
```

`--common-memory` and `--common-cpu` pass the same requested limits to every
pVisor case. `direct` remains an uncapped control. Select a case list without
`host_memory_limit` when using `--common-memory`, and confirm each executor's
effective limit in its Run Bundle; a common request does not imply identical
enforcement.

An adapter exits successfully only after its payload finishes. If a runtime
has asynchronous `start` semantics, write a wrapper that waits for the payload
and exits with its exit status; otherwise its timing is not comparable. Use
the same image contents, network policy, CPU and memory allocation, storage
medium, and warm cache state for each comparison. Keep `docker` daemon cost
and `pvisor` OCI runtime cost in scope if that matches the product question;
label such comparisons as end-to-end rather than isolation-only. Run on an
otherwise idle host with fixed CPU governor, no concurrent builds, and
pre-pulled images. Repeat the full run at least three times and compare p50,
p95, and run-to-run spread. Review any failure log and Run Bundle isolation
before accepting a result; never treat a fast failure as a sample. An external
daemon such as `dockerd` is outside the launcher process tree, so its CPU and
RSS are absent from this report. For cross-runtime resource comparisons,
measure all involved processes in a dedicated cgroup instead.

## Links

- [pVisor design](../../docs/src/zh/design/index.md)
- [`pvisor`](../../crates/pvisor/README.md)

## Guest init comparison (Apple Silicon)

`guest_init.py` embeds the old C init and the Rust guest in the same signed
libkrun runner, using release host libraries. It creates a fresh 1-vCPU,
128-MiB VM for every sample and interleaves both variants. Readiness ends at a
common payload's stdout marker; completion includes sync/reboot and VMM exit.
The payload stays alive for 20 ms after the marker so stdout can drain before
the VMM exits. Readiness excludes this delay; completion includes it.
Neither interval includes CLI preparation, image extraction, helper preparation,
or Run Bundle persistence. The C workspace case reconstructs the old shell
chain, using Alpine's `/bin/mount` BusyBox applet rather than a renamed copy.

Measured on 2026-10-01 with Apple M4/HVF, libkrunfw 5.5.0, Alpine
minirootfs 3.22.1 aarch64, 10 warmups and 100 samples per variant:

| Scenario | C ready p50 (ms) | Rust ready p50 (ms) | Rust ready p95 (ms) | Change in p50 |
|---|---:|---:|---:|---:|
| Direct command, network off | 105.27 | 114.57 | 116.63 | 8.83% slower |
| Workspace, network off | 119.18 | 114.48 | 116.24 | 3.94% faster |
| Workspace, network on | 119.46 | 114.81 | 116.33 | 3.89% faster |

The workspace cases save about 4.7 ms by removing the helper process chain;
replacing C with Rust alone does not improve the direct-command case. These
results describe this HVF runner, not full CLI startup or Linux/KVM performance.
The network case compares the old C DHCP setup with the Rust static address.

Provide a prepared Alpine aarch64 rootfs and firmware. To reproduce the old
C control without restoring its crate, extract the four sources from the
migration's parent commit and compile them with Zig (only the historical
benchmark control needs this C compiler):

```bash
mkdir -p target/guest-init-benchmark/c-src
for file in init.c dhcp.c dhcp.h jsmn.h; do
  git show "178445c^:vendor/krun-init-blob/init/$file" > "target/guest-init-benchmark/c-src/$file"
done
zig cc -target aarch64-linux-musl -O2 -static -Wall \
  target/guest-init-benchmark/c-src/init.c \
  target/guest-init-benchmark/c-src/dhcp.c \
  -o target/guest-init-benchmark/c-init
rustup target add aarch64-unknown-linux-musl
```

Then run the comparison:


```bash
python3 benchmark/pvisor/guest_init.py \
  --c-init target/guest-init-benchmark/c-init \
  --rootfs target/guest-init-benchmark/rootfs \
  --firmware target/libkrunfw/5.5.0-aarch64-apple-darwin-macos11.0/libkrunfw.5.dylib \
  --iterations 100 --warmup 10 --output target/guest-init-benchmark/release
```

Setup/build time is excluded. `results.json` contains every sample, medians,
p95, and hashes of the runner, both init binaries, firmware, and payload.

## Startup timing checkpoints

Host startup checkpoints are routine INFO diagnostics, enabled by default.
Capture ordinary CLI diagnostics from stderr:

```sh
./target/release/pvisor run \
  --vm --rootfs /path/to/prepared/rootfs --vm-library-dir /path/to/firmware \
  --overlaynet off -- /bin/sh -c 'printf "GUEST_READY\n"' \
  2>startup.log
```

Each `pvisor-startup` line includes `level`, `timestamp_ms`, `pid`, `ppid`,
`run_id`, `stage`, `monotonic_us`, and `process_elapsed_us`. Run IDs are JSON-quoted
to prevent newline injection; early process checkpoints use `run_id="-"`. Subtract `monotonic_us` values to measure intervals across
parent and runner processes on the same host; use PID/PPID to match the runner.
`process_elapsed_us` starts at each process's first checkpoint, not at
OS process creation. Executable loading and diagnostic routing initialization precede `process.entry`.
The switch is cached at first use and supported on Unix. `PVISOR_STARTUP_TIMING=0`
suppresses checkpoints for uninstrumented benchmarks; unset and `1` both log.
TUI Runs route parent and VM runner diagnostics into the existing frontend log,
using a pre-opened inherited descriptor for the runner. Complete log lines are
formatted before append so processes do not interleave formatting fragments. No extra fsync or
background logging service is introduced. Ordinary Runs use stderr; production
log collectors or `2>startup.log` retain it. Logging failures are best-effort.
Logs contain stage labels and timing/identity fields, not command arguments or
credentials. Guest clock timestamps cannot be subtracted from host timestamps.

| Checkpoints | Interval |
|---|---|
| `process.entry` → `cli.parsed` → `cli.runtime_ready` | CLI parsing and runtime setup |
| `cli.run_begin` → `cli.config_ready` | Config loading and CLI overrides |
| `cli.rootfs_begin` → `cli.vm_inputs_ready` | VM inputs and CLI preparation; includes more than rootfs alone |
| `session.begin` → `session.agentctl_ready` | Agent control setup |
| `session.storage_begin` → `session.storage_ready` | Runtime preparation, including storage/filesystem setup |
| `storage.overlay_begin` → `storage.overlay_ready` | Overlay preparation, when selected |
| `storage.record_write_begin` → `storage.record_write_ready` | Each preparation-time durable Run record write; may repeat |
| `session.storage_ready` → `session.events_ready` | Initial execution events publication |
| `vm.prepare_begin` → `vm.ram_backing_begin` | Executor inputs and root/workspace overlay preparation |
| `vm.ram_backing_begin` → `vm.ram_backing_ready` | RAM backing setup and exclusions |
| `vm.spec_write_begin` → `vm.spec_write_ready` | Private temporary runner specification write (no disk sync) |
| `vm.spawn_begin` → runner `process.entry` | Child launch and executable loading; overlaps spawn-return bookkeeping |
| `runner.spec_read_begin` → `runner.spec_read_ready` | Read/decode runner specification |
| `runner.context_begin` → `runner.context_ready` | libkrun context creation |
| `runner.context_ready` → `runner.devices_configured` | Guest config and device declarations |
| `runner.devices_configured` → `runner.attestation_ready` | Setup attestation write/sync |
| `runner.krun_enter` → `runner.vmm_built` | libkrun startup and VM construction |
| `cli.run_finished` → `cli.result_loaded` | Post-run bookkeeping through finalized record lookup |

`vm.spawn_returned` only means spawn returned to the parent; it does not mean the
runner has entered main. `cli.session_started` means the asynchronous session was
started, not that the guest is ready. `runner.vmm_built` is the libkrun ready
callback after VM construction; it is not an exact first-vCPU or guest-ready
marker. Log receipt order across processes can differ from timestamp order.

Use a payload marker and the external host timer for command-ready latency, as in
`firmware_boot.py`. These checkpoints do not invent a generic guest-ready event.
Capture-mode stderr may be delivered only after the run; timestamps still record
the actual checkpoints. Timing logs add diagnostic overhead, so measure final
latency with `PVISOR_STARTUP_TIMING=0`. No persistence or synchronization guarantees are
relaxed by this instrumentation.

## Full CLI → VM workload readiness (Apple Silicon)

`vm_ready.py` measures process creation → first workload stdout marker and
process creation → CLI exit separately. Each trial creates a new VM and validates
the completed Run Bundle and VM isolation after timing stops. Rootfs is prepared
and host caches are warm; image download, build and TUI are excluded.

```bash
python3 benchmark/pvisor/vm_ready.py \
  --binary target/release/pvisor \
  --rootfs target/guest-init-benchmark/rootfs \
  --official target/firmware-official-compare-20261003/official \
  --trimmed target/firmware-official-compare-20261003/trimmed \
  --output target/vm-startup-new \
  --samples 100 --warmups 5 --profile-samples 20
```

Both library directories must contain the intended `libkrunfw.5.dylib`.
The output must be new. The matrix includes native shell, pVisor host, and both
firmwares at 1/2/4 vCPU × 128 MiB and 2 vCPU × 2048 MiB. Diagnostic samples are
separate from the main matrix. `results.json` uses `pvisor-vm-readiness/v1`,
including raw samples, artifact SHA-256, source status and paired bootstrap
intervals. `samples.jsonl` is flushed incrementally; trial logs remain available.
The same shell command runs on host and guest, but macOS and Alpine use different
shell builds: host cases give context, not a pure virtualization-overhead estimate.

See the [startup article](../../docs/src/zh/benchmarks/startup.md) for measured
results, checkpoint boundaries, retained optimizations and limitations.

Render the startup article figures from its archived JSON (temporary plotting dependency):

```bash
uv run --no-project --with matplotlib python benchmark/pvisor/plot_startup.py
```

The plot script exports shared SVG assets and local PNG previews. Phase means
are checked to sum to observed diagnostic readiness. Historical optimization
comparisons use separate batches and are labeled accordingly.

## Linux VM lifecycle and full snapshots

The [VM memory benchmark](../../docs/src/zh/benchmarks/vm-memory/index.md#linux-lifecycle) records
real KVM pause/resume, plain and compressed RAM offload, and complete raw/compressed
snapshots. `vm_lifecycle_bench` verifies fixed and mutable guest RAM every cycle;
`vm_snapshot.py` terminates the source, removes its source trees and verifies two
private restored forks, open file offsets and continued guest execution.

```bash
cargo build --release --locked -p pvisor --bin pvisor --example vm_lifecycle_bench
target/release/examples/vm_lifecycle_bench \
  --rootfs / --firmware /path/to/libkrunfw-directory \
  --output target/vm-lifecycle-new --samples 30 --warmups 3
rustc --target x86_64-unknown-linux-musl -C opt-level=2 \
  benchmark/pvisor/snapshot_guest.rs -o /tmp/snapshot-guest
python3 benchmark/pvisor/vm_snapshot.py \
  --output target/vm-snapshot-new --binary target/release/pvisor \
  --guest /tmp/snapshot-guest --samples 10 --warmups 2
```

Outputs must be new directories on disk. The snapshot tool copies the CLI to pin
its compatibility digest and retains per-trial results and logs. Linux does not
support the VM shared cold-page pager; cross-process pool protocol tests do not
establish Linux guest reclamation or whole-host physical savings.

Snapshot RAM restore uses a read-only FUSE file and private COW mappings. New raw
snapshots carry a 64 KiB digest index (manifest v3); compressed snapshots reuse
the v2 block identities and decode only requested blocks. Legacy v1 raw snapshots
still verify the full RAM digest before mapping. Linux restore requires usable
`/dev/fuse` and mount permissions. Compare both restore-to-heartbeat latency and
the first complete RAM read: on-demand loading moves work into first access.
Full snapshot saves still read all RAM; decoded caches are per runner.

The hardware-backed FUSE test checks faults after snapshot deletion and GC, and
private writes across mappings. It is explicitly ignored by the ordinary suite:

```bash
cargo nextest run --locked -p pvisor --test environment_snapshot_lazy --run-ignored only
```

`vm_snapshot_lazy.py` additionally checks real KVM continuation after deleting
and collecting a snapshot before the first full heap read. It saves that restored
COW VM and restores it again, then checks mount cleanup after VM termination:

```bash
python3 benchmark/pvisor/vm_snapshot_lazy.py \
  --output target/vm-snapshot-lazy-new --binary target/release/pvisor \
  --guest /tmp/snapshot-guest
```

## VM correctness stress and fault injection

Run the Linux x86_64 KVM/FUSE gate with a **new** output directory:

```bash
just test-vm-stress target/vm-stress-new 5 4 20261004
```

This builds the CLI and a static multithreaded guest (requires the installed
`x86_64-unknown-linux-musl` Rust target). Missing KVM/FUSE permissions fail the
gate. The arguments select generations, concurrent forks and the random seed.
The seed selects the survivor and termination signals; OS scheduling is variable.
This is a correctness gate, not a latency benchmark.

Both raw and compressed snapshots must preserve 64 MiB of guest data, pagewise
COW mutations, four live worker threads, an open file's seek position, and a
partially consumed directory iterator. Every generation checks sibling and sealed
filesystem isolation, then deletes the parent snapshot and runs GC before another
full heap check and recapture. Faults include unsupported FIFO publication,
malformed/disconnected control clients, SIGTERM/SIGKILL of VM groups, and SIGKILL
after observing an in-progress RAM capture. A failed save must thaw the guest;
an interrupted save must leave the previous snapshot recoverable. Cleanup checks
actual `/proc/self/mountinfo`, control sockets, content pins and owned processes.

`events.jsonl`, per-VM logs and `result.json` retain failures, binary/guest digests,
the seed and cleanup evidence. Guest panics abort the process, including worker
panics; a live main thread cannot hide a dead worker. Snapshot regression coverage
also runs in `just test pvisor`: concurrent store initialization and parallel
authenticated reads/cache eviction racing GC do not require hardware.

The SDK lifecycle driver separately stresses racing pause/resume calls, an
abandoned offload caller, rejection of occupied backing paths, repeated RAM
reclaim and guest integrity. Cancellation can be tested while running, paused or
offloaded:

```bash
cargo build --release --locked -p pvisor --example vm_lifecycle_bench
target/release/examples/vm_lifecycle_bench \
  --rootfs / --firmware /path/to/libkrunfw-directory \
  --output target/vm-lifecycle-stress-new --samples 100 --warmups 2 --stress
target/release/examples/vm_lifecycle_bench \
  --rootfs / --firmware /path/to/libkrunfw-directory \
  --output target/vm-lifecycle-cancel-new --samples 10 --warmups 1 \
  --stress --compressed --cancel-while offloaded
```

Use `--cancel-while running` and `--cancel-while paused` in separate fresh runs to
cover the other terminal transitions. These Linux gates do not establish macOS
HVF behavior, active network connection recovery or host OOM resilience.

## Linux/KVM first-command readiness

`linux_vm_ready.py` reuses the first-output timing and completed-Bundle checks from
`vm_ready.py`, with native shell and host controls and 2-vCPU VM shapes at
128/256/2048 MiB. It retains logs and copies the CLI and firmware to pin identity;
output directories must be new. Samples exclude image preparation and use warm
host caches. This is separate from `startup.py` command-completion timing.

```bash
python3 benchmark/pvisor/linux_vm_ready.py \
  --binary target/release/pvisor --rootfs / \
  --firmware /path/to/libkrunfw-5.5.0-directory \
  --output target/vm-startup-linux-new --samples 100 --warmups 5
```

See the [startup article](../../docs/src/zh/benchmarks/startup.md#linux-results)
for retained macOS/HVF and Linux/KVM measurements and their environment differences.

## Product benchmarks, first release (Linux)

`product_v1.py` checks outputs and observed Bundle isolation before retaining a
performance sample. It pins CLI/firmware copies, snapshots harness source and
records hashes, input configuration, source dirtiness and failure diagnostics.
Reports use `pvisor-benchmark/v1`; output directories must be new.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --samples 30 --warmups 3 \
  --suites filesystem,network,apply,density,agent,isolation,replay,supervision,baselines
```

Prerequisites: Linux x86_64, accessible KVM/FUSE/user namespaces, GNU CLI with
`--vm-library-dir`, libkrunfw 5.5.0, Python 3.14 system stdlib, Git, ripgrep,
Rust, GCC, Node 24/npm, Podman and crun. `agent` additionally needs Claude Code
and Codex; it uses local deterministic responses with fake keys and no inference.
`replay` uses twenty synthetic native prefixes per adapter; it measures prefix
preparation, not real model fidelity. No real model or cloud account is used.

Run `--samples 1 --warmups 0` first. Full 100,000-file apply has substantial
CPU/disk cost and takes minutes per sample. It runs three independent stage
preparations concurrently, then times operations sequentially. `crashes` can
run separately and is also included by `apply`. Samples are capped at 10 for
1,000 files and 3 for 100,000; agent/replay/crashes at 3; density at 5 batches.
Agent, replay, supervision, density and baselines do not perform warmups; each
page gives its effective protocol. VM density has a host-memory guard.

`--oci-shape shell` creates a minimal idle-probe image; tools suites need the
default tools image. `--density-backends`, `--density-concurrencies`,
`--network-backends`, `--network-modes`, `--filesystem-backends`, `--filesystem-modes`, `--apply-sizes`
and `--vm-memory` isolate follow-up measurements. Keep each batch separate;
never pool incompatible rootfs or memory shapes. Local model/network servers
are owned and stopped by the harness. Trial payloads are removed after checks;
retain reports, fixture hashes, logs, Bundle evidence and the copied harness.

The [benchmark index](../../docs/src/zh/benchmarks/index.md) contains the retained
2026-10-04 first-version data and the earlier macOS/Linux VM measurements.

Recompute distributions without combining batches:

```bash
python3 benchmark/pvisor/summarize_product_v1.py \
  target/product-benchmark-new/report.json --output-csv /tmp/product-summary.csv
```

SIGKILL probes default to `--crash-files 10000`; `--crash-states committed`
can isolate the shortest transition window. Ledger state is read from the atomic
JSON tail during injection and fully decoded after death. A missed window fails
the injection rather than becoming a successful sample.

## Familiar runtimes and the complete Agent tool environment

`reference_baselines.py` measures native, pVisor host/staged/VM, a **private
rootless Docker Engine**, Firecracker PCI, QEMU q35, and QEMU microvm on Linux.
It runs a first-command probe, seven-tool self-check, seven filesystem/tool
workloads, a repair/test/diff workflow, and real Claude/Codex CLIs against a
local deterministic model. These are tool-environment measurements; there is
no paid inference, model success-rate benchmark, or SWE-bench claim.

Every available case gets 3 warmups and 30 samples, with seeded randomized
backend order. All execution trees use the same two host cores; Docker also
runs an in-container affinity helper because its runtime resets inherited CPU
affinity. VM cases use 2 vCPU, 128 MiB for the shell probe and 16 GiB for the
complete environment. Docker/native memory is not capped. RSS is process-tree
sum, not PSS; Docker includes its private daemon tree, whose idle cost is
recorded separately. The two cores are a common execution budget, not a claim
of identical isolation or complete resource policy.

Use an installed private rootless daemon on a short Unix-socket path; the
system daemon is neither required nor altered. Supply its host PID explicitly.
The runner checks its owner and socket before changing only that daemon's CPU
affinity. Linux/KVM/FUSE and installed Docker, Firecracker, QEMU, Claude, Codex,
Rust's musl target, Python/Node/Git/rg/GCC and e2fsprogs are prerequisites.
The automatic tool-base builder targets Fedora 44's Python 3.14/Node 24 layout;
`--tools-rootfs` accepts another already prepared equivalent base.

```bash
bash benchmark/pvisor/prepare_reference_kernel.sh \
  /absolute/path/to/clean/linux-6.12.109 target/reference-kernel-new
python3 benchmark/pvisor/prepare_reference_env.py \
  --output target/reference-env-new \
  --kernel-elf target/reference-kernel-new/vmlinux \
  --kernel-bzimage target/reference-kernel-new/arch/x86/boot/bzImage \
  --kernel-config target/reference-kernel-new/.config \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock
python3 benchmark/pvisor/reference_baselines.py \
  --assets target/reference-env-new --binary target/release/pvisor \
  --output target/reference-results-new \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock \
  --docker-root-pid 12345 --cpu-affinity 0,1 --samples 30 --warmups 3
```

Choose two allowed physical cores on your host. For a smoke check use
`--samples 1 --warmups 0`. Every output directory must be new. Preparation
measures offline copying, image import and ext4 creation separately; image
pulls and kernel compilation are excluded from timed jobs. Firecracker and
both QEMU cases use one kernel/config and private reflinked ext4 images;
pVisor uses its own embedded firmware and staged virtio-fs workspace. The
reference VMMs run without Firecracker jailer, systemd, SSH or cloud-init.
These configurations expose practical path costs, not a pure VMM or security
ranking. QEMU microvm is included so the PC machine is not used as its sole
performance representative.

Codex's inner sandbox is uniformly `danger-full-access` for these fixed,
controlled commands: the outer runtime owns the boundary. Default
`workspace-write` failures are recorded separately. A failed preflight stays
in `capabilities`, never becomes a fast latency sample, and makes the runner
exit nonzero after finishing unaffected cases. Guest panic, wrong task mode,
failed grading, absent tool results and wrong pVisor isolation are rejected.
Pinned binaries, actual guest kernel/tool versions, scripts, commands and
per-trial logs are retained. The method and interpreted results live in the
existing bilingual benchmark articles.

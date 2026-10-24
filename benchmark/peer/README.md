# Peer forwarding engineering experiments

Registry: [B-PEER-NET-ENG](../README.md#b-peer-net-eng). This is an engineering
comparison, separate from user-facing network benchmarks.

Build and freeze each source revision before comparing it:

```bash
cargo build --offline --profile performance -p pvisor-overlaynet --example peer_forwarding
mkdir -p benchmark/peer/.data/my-run
cp target/performance/examples/peer_forwarding benchmark/peer/.data/my-run/baseline
# Apply the candidate, rebuild, and freeze its binary separately.
cp target/performance/examples/peer_forwarding benchmark/peer/.data/my-run/candidate
python3 benchmark/peer/run.py --baseline benchmark/peer/.data/my-run/baseline \
  --candidate benchmark/peer/.data/my-run/candidate --output benchmark/peer/.data/my-run/results
```

The runner saves the complete predeclared randomized schedule, binary and source
hashes, frozen source, kernel, CPU information, affinity and subprocess outputs.
Freeze baseline source beside its binary before editing; the runner's source
snapshot describes the current candidate. Default affinity is the first four
allowed logical CPUs (override with `--cpus`). This is an affinity budget, without
an isolated cpuset, memory cap or CPU quota; unrelated host load can interfere.
Three warmup pairs precede 30 measurement pairs. Valid slow samples are retained;
there are no post-hoc exclusions. Any child failure invalidates the batch and is
saved before stopping. Do not merge batches. The summary reports medians and
paired bootstrap 95% intervals; an interval crossing zero means no detected
difference. Raw per-packet RTTs remain in each sample output.

Each child creates two public `VmNetwork` owners and real Unix datagram links in
a private temporary directory. Two backend threads and two guest drivers share
the same process. After 128 warmup round trips, it measures 2,000 64-byte Ethernet
ping/pongs, then 16,384 1514-byte frames in batches of eight; the receiver validates
every byte and replies with a cumulative batch acknowledgement. Goodput counts
forward Ethernet bytes, excludes framing and acknowledgements, and includes
per-batch acknowledgement waits and verification. CPU covers every thread in the
process during the bulk phase. Zero drops and exact packet counts are required.
Timeouts are failures, not zero-throughput samples. This bounded-window workload
does not measure maximum UDP offered load or imply lossless UDP under overload.

Chaos coverage is maintained separately in
`crates/pvisor-overlaynet/tests/peer_network.rs`; run it using
`JUST_TEMPDIR=/tmp just test pvisor-overlaynet`. Local socket access is required.
The tests cover stream fragmentation, restart/crash generations, congestion
isolation, simultaneous directions, and real smoltcp TCP recovery under
deterministic loss, duplication and reordering. No global network settings,
host bridge, TAP or privileged namespace is modified.

These experiments exclude virtio/KVM and do not compare Docker bridge, kernel
TCP, cross-process overhead, VM application throughput or cross-host networking.
Keep original logs, failures, binaries and analysis in `.data/`; report coverage
gaps explicitly.

To sample the default distribution profile independently, build the example
with `--release`, then run `run.py --candidate PATH --build-profile release
--output NEW_DIRECTORY`. Omitting `--baseline` records 3 warmups and 30 candidate
samples and reports their absolute levels. Keep this batch separate from the
paired comparison; it does not establish a performance ratio between profiles.

## Four real VMs versus Docker bridge

Registry: [B-FOUR-PEER-ENG](../README.md#b-four-peer-eng). The separate
`four_node.py` runner needs native Linux/KVM, local Docker, a user systemd manager,
CPU 4–9 and a release CLI with working local firmware. It creates only dedicated
resources. Build the dependency-free worker with:

```bash
rustc --edition=2024 -O --target x86_64-unknown-linux-musl -C linker=rust-lld \
  benchmark/peer/four_node_worker.rs -o /PRIVATE_CONTEXT/netbench
```

Use the same `FROM scratch` / `COPY netbench /netbench` context for both engines.
Build the pVisor image in a new short `PVISOR_DOCKER_HOME` directory and the Docker
image under a unique tag. Create a dedicated network in each engine using
`10.231.70.0/24`; first check that the subnet does not conflict with host routes.
Set Docker's `com.docker.network.driver.mtu=1500`. Freeze the release CLI and
`libkrunfw.so.5` beside it. Freeze all artifact identities before preflight.

The config JSON supplies `prefix`, absolute `pvisor` path, private `store`,
`pvisor_network`, `docker_network`, `pvisor_image`, `docker_image`, `daemon_pids`
(dockerd/containerd name-to-PID map), and `artifact_sha256` (absolute input paths
to hashes). The raw experiment's `config.json` is a concrete example. Run:

```bash
python3 benchmark/peer/four_node.py --config CONFIG.json --output NEW_PREFLIGHT --preflight
python3 benchmark/peer/four_node.py --config CONFIG.json --output NEW_COHORT
```

Every condition starts four fresh instances and destroys them afterwards.
Each pVisor scope has a 512 MiB total host-memory hard limit, zero swap, one CPU
quota and task affinity to logical CPUs 4–7; guest RAM is configured as 384 MiB
to leave space for VMM/network overhead. Docker containers have 512 MiB/zero
swap/one CPU quota and cpuset 4–7. The user subtree may lack cpuset delegation;
all actual scope thread affinities are checked. Limits, guest memory/MTU and
OOM/max events are validated. The controller and sampler run on CPU 8–9.

The RPC case has all 12 directed links, 500 256-byte echo requests each; bulk
sends 4 MiB on each link using 64 KiB buffers; ring sends 200 256-byte requests
from each node through four guest application servers and back. Connections are
established before the four-client GO barrier. Every byte and response is
verified. Goodput is total forward application payload divided by the slowest
client duration. Ring counts original payload once, not each hop. CPU counts
all four instance cgroups during the operation; it includes guest kernels/VMM
for pVisor, and does not include Docker's shared host kernel globally. Monitor
samples are every 50 ms, while kernel `memory.peak` retains shorter peaks.
Startup, image preparation, client launch and barrier waiting are outside the
network timings. Cold disk, startup performance and DNS lookup latency are not
measured. Source, schedule, raw RTTs, cgroups and cleanup evidence are retained.

Preflight additionally validates all 12 DNS directions, a one-second slow
reader during healthy traffic, healthy three-node RPC while node 1 is stopped,
and full-mesh recovery after restarting it. Formal sampling has 3 warmup pairs
and 30 randomized pairs, fixed rpc/bulk/ring case order, no slow-sample exclusions
and paired bootstrap intervals. Failures stop the batch and retain diagnostics.
Do not run builds, tests or another experiment concurrently. The rootful Docker
daemon/containerd are existing host services outside these budgets; their CPU
and RSS snapshots are reported separately, not silently charged as zero. Clean
up only this experiment's networks/images/store after preserving evidence.

### Comparing peer forwarding implementations

`four_node.py --compare-pvisor /FROZEN_BASELINE/pvisor` runs candidate/baseline
four-VM groups with the same resource and sampling protocol. Set `pvisor` in the
config to the frozen candidate. Include both binaries, firmware and the identical
worker in `artifact_sha256`. Optional `runtime_source` selects the frozen candidate
source tree for provenance; keep the baseline source archive separately. Freeze
only the intended changes onto the baseline source when the main workspace has
other concurrent edits. This comparison does not use the older Docker cohort to
calculate optimization percentages. Differences are candidate minus baseline.

```bash
python3 benchmark/peer/four_node.py --config CONFIG.json --preflight \
  --compare-pvisor /FROZEN_BASELINE/pvisor --output NEW_PREFLIGHT
python3 benchmark/peer/four_node.py --config CONFIG.json \
  --compare-pvisor /FROZEN_BASELINE/pvisor --output NEW_COHORT
```

Set `reject_foreign_work: true` for optimization cohorts. Admission and a 0.5 s
sampler reject visible same-user builds/tests or foreign KVM processes, retaining
the whole failed cohort without killing unrelated work. Unreadable processes
remain a coverage gap. Instance records, completion and logs are copied before
owned resources are removed, including when startup fails without guest output.

An independent sustained-throughput cohort can use `--cases bulk-long`: each of
12 directions sends 32 MiB (384 MiB total). Rebuild the worker and image for both
arms together and freeze their identities. Optional `tcp_counters: true` saves
guest `/proc/net/snmp` before and after the operation, outside the network and
cgroup CPU timing windows. Do not combine this cohort with default 4 MiB samples
or extend sampling until an interval becomes significant. A startup failure also
invalidates the whole cohort; completed traffic remains diagnostic evidence.

Linux GNU diagnosis can compile `trace_syscalls.c` with
`cc -shared -fPIC -O2 -Wall -Wextra -Werror -pthread` and set `profile_library` in
the config. The library is injected only into owned test processes; `--preflight`
is mandatory and formal timing rejects this setting. It counts per-thread
socket/read/write/epoll calls, EAGAIN, bytes and elapsed syscall wall time. It is
not a CPU stack profiler, and wall time includes blocking/descheduling. It uses
raw syscalls instead of libc cancellation wrappers and is diagnostic-only.
`sendmmsg`/`recvmmsg` additionally report successfully processed `messages`;
`bytes` remains payload bytes, not the returned message count. Compare successful
message counts with calls excluding EAGAIN to inspect actual batch sizes.
Runners can exit without destructors, so cumulative checkpoints are emitted
every 1,024 calls: take the maximum per pid/tid/op, never sum checkpoints; the
unflushed tail is a lower bound. Remove `profile_library` for the uninstrumented
correctness preflight and formal comparison.

Results and limitations: [forwarding optimization report](PEER_OPTIMIZATION_REPORT.md).
The subsequent Linux mmsg and VMM TX gather implementation is recorded separately
in [the packet batching report](PACKET_BATCH_REPORT.md), including its invalid
formal cohort; do not combine those samples with the earlier optimization.
The fresh four-VM versus Docker comparison of that batched implementation is in
[the batched Docker report](FOUR_NODE_BATCHED_DOCKER_REPORT.md); its complete
cohort and source identities are separate from both earlier comparisons.

## Sustained correctness and fault injection

Registry: [B-PEER-CHAOS-ENG](../README.md#b-peer-chaos-eng), diagnostic. The question
is whether persistent instances preserve packet/application integrity and isolate
an unavailable peer during a fixed observation window. This is independent of
performance sampling and does not establish production reliability.

`chaos.py` uses a frozen CLI/firmware and the same four-node resource limits.
Build `four_node_worker.rs` as `/netbench` and `udp_worker.rs` as `/udpbench` using
the static musl command above; include both in a new scratch image. Use a private
store, unique prefix and dedicated user-defined network. Freeze the exact image
catalog, both workers, context, runtime source and binary hashes before running.
The config uses the pVisor fields from `four_node.py`; Docker is not needed.

```bash
python3 benchmark/peer/chaos.py --config CONFIG.json --output NEW_PREFLIGHT --seconds 30 --faults 4
python3 benchmark/peer/chaos.py --config CONFIG.json --output NEW_SOAK
python3 benchmark/peer/audit_chaos.py NEW_SOAK
```

The default is 1,800 seconds with 30 faults scheduled at equal offsets, rotating
three-second cgroup freeze, normal stop/start, SIGKILL of the owned KVM process
via pidfd, and removal/recreation with the same name/IP. The other three VMs stay
alive. Healthy traffic must complete inside every fault window. The freeze starts
after the full-mesh connection barrier; all established TCP requests must finish
after thaw. Crash/recreation requires new connections and all 12 DNS directions
to recover; old crashed sessions are not expected to survive.

Traffic cycles through full-mesh RPC, 384 MiB full-mesh bulk, four-hop RPC and a
slow-reader case. Every cycle also checks 12 UDP directions with 32–1,472-byte
payloads, unique per-run tokens and full-byte verification: 180 requests per flow,
at most eight attempts each, and 250 ms receive timeouts. Retries and late duplicate
echoes are reported, not hidden. Three additional TCP connections among the
healthy nodes remain open for the observation window, continuously exchanging
variable-length, sequenced and byte-checked payloads.

The runner saves all stdout/stderr, one-second cgroup/FD/thread samples, original
owner records before restart/removal, final cleanup and a frozen harness. Any
unexpected failure stops the cohort without a retry; failed preflights are kept
separately. Host build/KVM activity is recorded, not killed; this correctness
experiment makes no performance claims. Review resource time series and persistent
PID/start-time identities as well as the summary. `--seconds` sets a minimum window,
so a final in-flight operation can extend it. Clean only the owned network and
instances after preserving evidence. Malformed frames, spoofing, isolation and
deterministic loss/duplication/reordering remain separate public-interface tests
in `peer_network.rs`, not claims from the real-guest application probe.

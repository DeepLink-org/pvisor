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

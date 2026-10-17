# Memory experiments

Read the [benchmark registry](../../benchmark/README.md) and
[table framework](../README.md) first. `scanner_compare.py` serves the
B-MEMORY-SCALE engineering extension; results do not populate user benchmarks.
The other null JSON templates remain unprepared and are not elapsed-time
substitutes for phase memory accounting. `scanner_compare.json` is a Python paired
row; `--prepare` expands its inputs, and `semspec bench` executes the coordinator.

## Adaptive page scanner versus KSM

Question: at the same 60-second observation window, how much memory does the
pVisor UKSM-inspired physical page pool save relative to default-parameter kernel
KSM, and what CPU work accompanies that saving?

This measures pVisor's implementation, not a kernel with the upstream UKSM patch.
The three arms are private anonymous RAM without advice, the same RAM with KSM
advice, and the daemon physical page pool. Modern KSM's `smart_scan` stays enabled.
Do not change host sysfs from the coordinator. An administrator must configure
KSM before the full comparison. Record all tunables; a run with changed values
must not be called default KSM.

Each condition has four new 256 MiB/one-vCPU VMs, each with a fully verified
64 MiB payload. Repeated pages, identical random pages across VMs, and unique
random pages are separate conditions. The product group includes the SDK, VM
processes and daemon, with four CPU quota units, affinity 0–3, 2 GiB memory and
zero swap. All-ready starts the common 60-second window; scanners can also work
during sequential startup. This is not a synchronized scanner-start experiment
or proof of convergence. The existing worker then checks 25% and 100% private
writes, unchanged peers, independent exit and survivor integrity. Post-write
snapshots do not use an additional 60-second convergence window.

PSS includes daemon/process mappings; cgroup memory also captures charged cache
and kernel memory. Neither alone measures all host costs. Whole-group CPU during
the scan window includes guest heartbeat and observer work. Global ksmd CPU is
recorded separately across admission and the whole attempt, outside the group's
quota; it cannot be attributed exactly to this group or compared as a pure
algorithm CPU cost. Global KSM page counters likewise are not group savings.
The native worker verifies positive accepted advice and mergeable VM mappings.

Preflight uses one fresh group per condition. Formal sampling uses at least five
rounds with predetermined randomized condition order, no speed-based exclusion,
30-second quiet admission and continuous foreign VM/build checks. Any integrity,
budget, interference or cleanup failure stops the campaign and retains evidence.
Preflight and incomplete cohorts cannot supply formal comparisons. Inspect raw
distributions before using medians; separated distributions require cluster
reporting. Paired bootstrap intervals use 5,000 resamples. No tail latency or
production-density claim is supported. No upstream UKSM versus KSM claim is
supported, and the daemon comparison includes architecture costs beyond scanning.

Build the release `vm_memory_scale` example and `pvisor-daemon` before sampling.
Freeze them as `probe` and `pool-daemon` in a build directory with `source.tar.gz`
and `build-receipt.json`. The receipt must bind `example_sha256`, `binary_sha256`
and `source_archive_sha256`, and record the actual build command/compiler/source
identity. Use identical prepared rootfs and firmware inputs for every condition.

```sh
python3 benchmark.new/memory/scanner_compare.py \
  --build benchmark.new/memory/.data/build-20261018 \
  --rootfs benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware benchmark.new/memory/.data/build-20261018/firmware \
  --output benchmark.new/memory/.data/p2 --preflight
# After every preflight condition passes, select a NEW short directory.
python3 benchmark.new/memory/scanner_compare.py \
  --build benchmark.new/memory/.data/build-20261018 \
  --rootfs benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware benchmark.new/memory/.data/build-20261018/firmware \
  --output benchmark.new/memory/.data/f1 --samples 5 --prepare
target/semspec/debug/semspec bench --cwd "$PWD" \
  benchmark.new/memory/.data/f1.input.json
```

`--without-ksm` creates a separate off/adaptive subset when the host scanner is
unavailable. It does not establish a KSM comparison and must not be pooled with a
later full cohort. Keep all raw mappings, monitors, guest logs, source/binary
receipts, frozen harness and summary under `.data/`. Do not rebuild during
sampling. The worker output path must be at most 70 bytes and on real disk.

Conventional harness checks, performed outside sampling:

```sh
just test-py discover -s benchmark.new/memory -p test_scanner_compare.py
```

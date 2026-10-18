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

## Firecracker and pooled-memory curves

### Docker comparison under preparation

The requested Docker baseline uses a private rootless Docker daemon and four
containers in one owned systemd slice, including dockerd, containerd, shims and
attach clients in the same 4-CPU/2-GiB/zero-swap budget. Each container has a
256-MiB limit and one CPU quota. It shares the host kernel; its image page cache
and daemon costs differ from a VM. The existing host Docker service is not the
measured daemon. Record application PSS separately from complete-group costs.
This is a rootless Docker configuration, not evidence for every Docker setup.

`prepare_docker.py --inputs FROZEN_POOL_INPUTS --output NEW_DOCKER_INPUTS` imports
the exact frozen Python userspace, `.so`, library worker and static byte oracle,
plus `docker_init.rs`, which pins CPUs 0–3 before exec. This launcher is necessary
on this host because rootless Docker lacks delegated cpuset control; requested
Docker flags alone are not proof of placement. Actual host PID, affinity,
container limits, image identity and recursive cgroup/PSS evidence are required.
Rootless resource limits require cgroup v2 and systemd; see the
[Docker resource-limit documentation](https://docs.docker.com/engine/security/rootless/tips/#limiting-resources).
Each condition gets a private reflink copy of a stopped image-store seed and
fresh containers, with network disabled and a read-only image. Preparation and
image import occur before measurement; daemon startup remains in startup curves.
The input receipt binds image/archive bytes, source bytes and runtime binaries.
Docker qualification and randomized measurements are still pending.

The first complete Docker qualification (`.data/dq1/`) hit the old 128-task
ceiling while starting the third container and is retained as failed. Protocol
v4 gives **every arm** a 256-task ceiling, retaining 4 CPUs, 2 GiB and zero swap;
the actual cap is checked in internal and external evidence. This admits the
dedicated Docker runtime's startup threads without changing memory/CPU limits.
Historical v2/v3 observations keep their exact 128-task checks and cannot supply
the new formal cohort. `.data/q6/` was deliberately cancelled when Docker was
requested; it is partial evidence, not a performance comparison.

`.data/dq2/` reached four verified ready payloads but failed complete PSS
collection because the unused pasta networking helper denied proc reads.
The memory workloads use no networking, so RootlessKit now uses its explicit
`none` network and port drivers as well as Docker `--network=none`. A separate
daemon diagnostic in `.data/dc3/` verifies readable PSS for every runtime process;
no host proc permissions or helper security settings were changed. Both failed
qualifications remain excluded from performance summaries.

`pool_compare.py` uses stock Firecracker as the primary baseline. All arms run
the same static 64 MiB worker, including rootless Docker: no Python payload, periodic file heartbeat or
SDK checkpoint producer. Supplemental pVisor arms are off, KSM, physical pool,
sealed compressed pool and local compression. Different guest kernels,
virtio-blk/virtio-fs root paths and the pVisor guest supervisor remain explicit
product differences. Stock Firecracker's unsupported KSM option is not injected.

The parent provides one four-CPU/2 GiB/zero-swap cgroup per four-VM group. An
external observer captures charged memory every 50 ms and PSS every 500 ms;
actual timestamps, missing reads and all failures are retained. Daemon mappings
are included. The guest waits for stdin between full byte/SHA-256 checks. Ready,
1/2/5/10/20/40/60-second snapshots, first full readback, 25/100% COW and survivor
checks are separate. Profile v3 additionally holds all four VMs for 30 seconds
after full private writes and peer checks, with 1/2/5/10/20/30-second snapshots.
This distinguishes delayed old-slot release from persistent metadata overhead;
v2 cohorts retain their original immediate post-write protocol and are not pooled
with v3. Scanners may work during startup. CPU includes validation
and sampling work inside the group; external monitor and global ksmd are outside.
Compression also changes granularity and restore path, so this matrix alone does
not isolate codec CPU. Physical mode never compresses payloads.

Inputs must be frozen under a NEW `.data/` directory: `probe` is the release
`pool_memory_case` example, `daemon` is `pvisor-daemon`, `firecracker` is stock,
`kernel` is its reference ELF, `firmware` is libkrunfw, and `worker` is the static
`firecracker_ksm.WORKER` executable. `root/worker` contains the same executable;
`guest.ext4` is a read-only boot disk built from that root. `receipt.json` binds
all input hashes and compiler/source/build provenance. Never rebuild while
sampling. Harness files are frozen and checked before every condition and at end.
The read-only pool FD survey records unique memfd allocated blocks separately
from its logical reservation; these are storage diagnostics, not net savings.
The static-worker comparison clears inherited `PVISOR_MEMORY_FILE_HINTS` in
child environments so the frozen product's default policy is measured; explicit
hint-off/on controls belong to the separate library cohort.

Prepare reproducible current-source inputs before any VM sampling:

```sh
python3 benchmark.new/memory/prepare_pool.py --output /short/new-inputs \
  --firmware /absolute/libkrunfw.so.5 --kernel /absolute/fc-vmlinux
```

The helper archives actual repository build inputs, records dirty state and
checks they stay unchanged across the targeted release build. External Cargo
registry sources are not archived; exact firmware and kernel bytes are retained.
To use a source-built stock Firecracker, pass `--firecracker`,
`--firecracker-filter` and `--firecracker-build-receipt`. The receipt must match
the binary and supplied filter hashes; retain its source/build provenance.
Optional `--library-rootfs DIR` also prepares a Python root, its complete
file/link/mode manifest and read-only Firecracker `library.ext4`, and an aligned
64 MiB `.so` from `library_payload.c`, plus `library_worker.py`. The immutable
`libraries/` directory contains only the read-only library; stages/logs remain
outside it. Use it with `pool_memory_case --mode off --libraries DIR`, optionally
`--buffered-libraries`. The loader checks STATX DAX, full SHA-256 and private
25/100% writes. This is an explicit attachment correctness path, not automatic
daemon rootfs-library discovery.

`library_compare.py` separately compares actual `dlopen` through Firecracker
virtio-blk (stock Firecracker), rootless Docker, ordinary pVisor rootfs (off, KSM,
physical pool with file READ hints off/on) and an explicit immutable DAX
attachment. The two hint arms use one binary and explicit host-only `0`/`1`
settings; normal rootfs reads remain mutable and are never promoted to DAX.
All seven arms use the same prepared Python userspace and exact library bytes,
four 256 MiB VMs or limited containers, 60-second idle and 30-second post-write windows and all 58
full-payload/COW/peer/exit checks. Prepared input cache is retained; no global
drop-cache operation is used. This is a separate cohort from the static worker.
Prepared source cache may be charged to its preparer outside the product group.
PSS includes mapped DAX pages regardless of charge ownership; report both curves,
and do not treat cgroup memory alone as whole-host physical savings.

After idle timing and timed first full readback, a bounded observer outside the product cgroup checks target
library pages against complete canonical bytes and rechecks present/file PTEs,
inode and file offsets without reading PFNs. It reports target pages sharing
the same slot across all four VMs. Counts are conservative observations, not
net memory savings or an atomic VM snapshot.
The content match identifies controlled library data in guest RAM; it does not
walk guest page tables to attribute each match to an application virtual address.
The additional audit wait is recorded separately, before private writes, and
cannot prewarm the timed first readback. No target sharing in a physical
arm is valid zero benefit; a DAX arm without any shared target page fails its
data-path check. Guest STATX DAX attributes, rootfs/source hashes and all failed
conditions are retained. Use the common preparation helper's `--library-rootfs`
option, then the independent semspec input:

```sh
python3 benchmark.new/memory/library_compare.py --inputs /short/frozen-inputs \
  --docker-inputs /short/frozen-docker-inputs \
  --output /short/new-library-preflight --preflight --prepare
target/semspec/debug/semspec bench --cwd "$PWD" /short/new-library-preflight.input.json
```

Formal library sampling requires all seven variants and at least five randomized
rounds in a NEW directory. It cannot be substituted for the three-content
static-payload matrix or pooled with its results.

```sh
python3 benchmark.new/memory/pool_compare.py \
  --inputs benchmark.new/memory/.data/pool-build-5 \
  --docker-inputs benchmark.new/memory/.data/docker-inputs-1 \
  --output benchmark.new/memory/.data/q2 --preflight --prepare
target/semspec/debug/semspec bench --cwd "$PWD" \
  benchmark.new/memory/.data/q2.input.json
```

Full preflight covers all seven arms and three patterns. Subsets are diagnostic
only. A new formal output requires all arms/patterns and at least five randomized
rounds. Preserve failed and changed-harness cohorts; never use them for a formal
comparison. `just test-py discover -s benchmark.new/memory -p test_pool_compare.py`
checks missing integrity acknowledgements, wrong digests, budget/swap failures,
short scan windows, incomplete ownership and unexpected stock-Firecracker advice.

`plot_pool.py` audits complete N=1 diagnostic cohorts. Historical cohorts with
retired conditions require an explicit `--arms` selection of supported conditions.
This produces a separate
projection, records included/excluded identities and the original report hash,
and leaves raw measurements unchanged. Projection is forbidden for formal
analysis; new formal cohorts must match the current seven-arm matrix exactly.
For a separate complete formal cohort,
`analyze_pool.py REPORT --output NEW_DIRECTORY` requires every
condition in at least five randomized rounds and current placement/KSM evidence.
It retains all samples, per-process/global-ksmd CPU, observed boundary timestamps,
individual curves and medians. Differences are paired by round, with 5,000 paired
bootstrap resamples; a negative difference means lower memory/CPU/read cost.
Separate startup curves retain initial group setup through the all-ready
snapshot, with actual timestamps and a visible readiness marker. This matters
when file hints reclaim pages during loading, before the 60-second idle window
starts. Ready footprint and setup-to-ready duration are reported separately;
an unreached additional 128 MiB idle drop is still censored, even if sharing
already occurred during startup.
Inspect individual distributions and curves for clusters before drawing a
conclusion. Global ksmd cost remains separately scoped, outside the group quota.
Neither renderer accepts incomplete or failed cohorts for aggregate conclusions.
The sampled time to a sustained 128 MiB drop is bracketed by actual observations,
relative to that condition's all-ready footprint. Unreached targets remain
censored, never imputed as zero or 60 seconds. This is whole-group memory,
including kernel/runtime differences; it is not application-only reclaim or an
exact convergence time. Per-process CPU separates VM, pool and control/sampling
work; process counters have clock-tick resolution. The external observers remain
outside this accounting. Whole-condition kernel peak and sampled idle peak are
separate metrics. Formal analysis also rechecks all input/build/source receipts,
frozen supporting harnesses and the hashes of monitor, PSS and guard observations.

### Retained first curve diagnostic

Local `.data/q1/` retains one random-shared group per condition, four fresh VMs
each. [The curve](.data/q1/derived2/memory-curves.svg) and derived JSON are audited
by `plot_pool.py`; it rejects incomplete groups, altered reports and missing
payload/ownership evidence. This N=1 subset has no confidence or tail claim.

| Condition | Ready charged MiB | 2 s charged MiB | 60 s charged MiB | 60 s PSS MiB | Idle group CPU s | First full group read ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Stock Firecracker | 365.0 | 365.5 | 365.3 | 366.1 | 0.150 | 646.8 |
| pVisor physical pool | 405.8 | 223.0 | 214.6 | 212.6 | 9.291 | 902.7 |
| pVisor sealed compressed pool | 413.6 | 418.2 | 375.6 | 385.4 | 2.789 | 650.0 |

Physical sharing reduced the observed footprint quickly, but consumed more CPU.
The full read includes byte validation, hashing and transport, not just a fault.
Sealed compression rejects poorly compressed blocks, so similar read timing
does not mean equally much application payload was reclaimed. A separate local
two-session diagnostic (`pool-build-2/codec-check/result.json`) submits the same
64 KiB random payload twice: both are rejected with insufficient compression
savings and zero objects, proving this admission limitation. It does not
attribute the whole VM's footprint to that block.

These curves precede the cached-session-count and bulk-pagemap optimizations;
they do not establish either optimization's performance or a KSM speed win.
The KSM curve, full content matrix and randomized repeats remain required.
Four-VM buffered and DAX library checks each passed 54 full-hash/isolation/exit
acknowledgements; a separate two-VM smaps check confirmed the same library inode.
Their receipts live in `.data/library-inputs-1/`; they are correctness evidence,
not a Firecracker library-load performance comparison.

### Static-worker curve preflight

The retained six-arm projection of `.data/q3/` v2 contains 18 conditions and
1,044 full-content, COW/peer and exit acknowledgements without OOM, swap or
cleanup failure. Historical raw files retain the retired control; it is excluded
from the tables and regenerated figures below. Each
cell is N=1, with no confidence or production-density claim. The host is Linux
7.2.10-200.fc44.x86_64 on Ryzen 7 9700X; KSM uses the recorded default parameters,
including smart_scan=1. Each condition has four 256 MiB/one-vCPU VMs and the same
64 MiB static payload, in a four-CPU/2 GiB/zero-swap product group.

Stock Firecracker is an unmodified v1.13.1 musl build with an explicit official
seccomp filter. `.data/fc-build-2/receipt.json` retains build provenance.
The installed-binary q1 cohort remains separate. pVisor inputs/source evidence
are frozen in `.data/pool-build-3/`. This build includes the cached-session-count
and bulk-pagemap changes, before the later present-only selection/authority fix.

[Charged-memory and PSS curves](.data/q3/derived-current/memory-curves.svg) and
[audited derived metrics](.data/q3/derived-current/summary.json) retain the raw observations.
The following values are complete-group charged MiB at 60 seconds:

| Content | Firecracker | pVisor off | pVisor KSM | Physical pool | Sealed compression | Local compression |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Repeated pages | 364.8 | 398.3 | 113.5 | 148.3 | 185.7 | 116.9 |
| Same random pages across VMs | 364.8 | 398.4 | 176.2 | 221.7 | 376.7 | 372.1 |
| Unique random pages | 365.0 | 398.5 | 368.0 | 390.6 | 376.7 | 372.7 |

For same random content, the physical pool's sustained 128 MiB charged-memory
drop is observed between 1.67–1.72 seconds and pVisor KSM between 27.72–27.77
seconds. These are sampled brackets
relative to each condition's ready footprint, not exact payload convergence or
a generalized speedup. At 60 seconds KSM has the smaller footprint.

The physical pool consumes 9.10 CPU seconds in VM/pool processes during the idle
window, versus 0.28 for pVisor KSM; global ksmd separately consumes 0.48 seconds
in the KSM attempt. Group CPU also includes control/sampling work: 11.35 and
0.39 seconds respectively. Global ksmd is outside the group quota and cannot be
attributed exactly. External observers are outside these totals.

Physical mode performs no compression. Its same-random group has about 72,800
pool VMAs, 16.13 MiB slab and 20.52 MiB total kernel accounting; pVisor KSM has
1.68 MiB slab and 5.59 MiB kernel accounting. VM system CPU continues to grow
after mapping counts stabilize, motivating a single pagemap state query instead
of separate mincore/pagemap walks. This is an observed cost breakdown and a
source-guided hypothesis, not isolated syscall attribution.

The first full read preserves the sharing: physical charged memory changes from
221.7 to 222.0 MiB and pVisor KSM from 176.2 to 176.1 MiB. Immediate full-private-
write observations are 485.2 and 367.9 MiB respectively, but v2 has no extra
settling window, so these do not establish persistent post-write overhead.

Subsequent source changes skip non-present PTEs during selection and require a
present anonymous PTE before retiring an old shared-slot reference. A deterministic
absent-file-PTE publication regression fails before that authority fix and passes
after it; 40 targeted VM/KVM/API tests pass. The v3 harness and audit/statistical
gates pass 19 Python tests. The changed product's performance and the new 30-second
post-write curves require a separate cohort; do not attribute q3 results to it.

### Present-only selection diagnostic

`.data/q4/` separately validates the changed physical pool with v3 inputs from
`.data/pool-build-4/`: all three content conditions, 174 full acknowledgements,
the 30-second write-settling window and cleanup pass. This is a physical-only
N=1 diagnostic, not a new full matrix or a paired causal comparison with q3.
[Idle curves](.data/q4/derived/memory-curves.svg),
[write-settling curves](.data/q4/derived/post-write-curves.svg) and
[derived observations](.data/q4/derived/summary.json) retain their own source.

| Content | 60 s charged MiB | VM + pool idle CPU s | Immediate full-write MiB | +10 s MiB | +30 s MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Repeated pages | 147.7 | 5.42 | 416.9 | 420.3 | 420.1 |
| Same random pages | 221.3 | 5.99 | 484.4 | 427.5 | 427.7 |
| Unique random pages | 390.2 | 8.62 | 393.2 | 397.6 | 398.0 |

Same-random readback retains sharing (221.3 to 221.6 MiB). Its old pool storage
falls from 64.6 MiB immediately after private writes to 6.8 MiB by ten seconds,
while logical reservation remains 128 MiB; this proves delayed slot retirement
accounts for part of the immediate peak. The remaining charged footprint is
not zero-overhead sharing. Unique random content still incurs substantial CPU
with little memory benefit, so poor-region/unchanged-page work remains a target.
Do not use the separate q3/q4 single observations as a confidence interval or
an isolated percentage improvement. Full v3 randomized repeats remain required.

### Complete write-settling preflight

The retained six-arm projection of `.data/q5/` covers three contents using the
same present-only/authority-fixed `.data/pool-build-4/` inputs. Its 1,044 full-content,
COW/peer and exit acknowledgements and resource/cleanup gates pass. The retired
service is inactive; this is a separate v3 N=1 diagnostic, not five-round formal
evidence or a causal comparison with older binaries.

Audited [idle curves](.data/q5/derived-current/memory-curves.svg),
[startup observations](.data/q5/derived-current/startup-memory-curves.svg),
[post-write curves](.data/q5/derived-current/post-write-curves.svg) and
[derived metrics](.data/q5/derived-current/summary.json) retain the six selected arms. The historical raw cohort remains unchanged.
Same-random content has the following complete-group observations:

| Condition | Idle 60 s charged MiB | Idle VM + pool CPU s | Idle group CPU s | Immediate full-write MiB | Write +30 s MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Firecracker | 364.90 | 0.12 | 0.163 | 365.82 | 366.33 |
| pVisor KSM | 176.70 | 0.30 | 0.375 | 368.82 | 369.94 |
| Physical pool | 221.46 | 4.98 | 7.160 | 484.40 | 427.75 |

Global ksmd separately consumes 0.41 CPU seconds for pVisor KSM; it is outside
quota and not exactly attributable.
Physical sharing's sustained 128 MiB drop is observed at 1.678–1.729 seconds,
pVisor KSM at 27.710–27.761, relative to
each all-ready footprint. KSM retains the lower 60-second footprint; physical
mode retains higher CPU and post-write overhead. Unique-random physical mode
still costs 8.17 VM/pool CPU seconds with little saving.

Subsequent source adds successful-file-READ candidate hints and groups probes in
bootstrapped poor regions. These changes pass the complete VM package (527
passing tests, eight skipped), including native KVM, COW and public boundary
checks, and 30 memory-harness/target-evidence/statistical Python tests. Rustdoc
passes with warnings denied. VM library Clippy passes with its existing
`unnecessary_cast` allowance; the all-target run remains blocked by pre-existing
`err_expect`, `useless_conversion` and example `chunks_exact_to_as_chunks`
findings in untouched files. The helper and daemon consumers compile. Logs are
`.data/pool-check-4-*`. The changed source has not yet supplied a performance
cohort; q5 must not be attributed to it. Shared-library preflight and randomized
formal comparisons remain required.

The next frozen `.data/pool-build-5/` contains the validated file-hint and grouped
scanner changes, plus common Python/rootfs/ext4/library inputs and C-toolchain
identities. `.data/lcheck5/` retains two four-VM ordinary-rootfs correctness runs
on that exact binary. Hints enabled yields positive candidate counters in every
VM (12,809 / 12,281 / 11,250 / 9,538) and 16,376 of 16,384 target pages observed
on common slots across all four VMs. Hints disabled yields zero counters in all
VMs, with all 16,384 target pages shared by the background scanner when audited.
Each run passes all 58 full-content/COW/peer/exit checks; the source library is
unchanged and both owned units are inactive. Hints are not needed for eventual
sharing. These runs enable metrics and collect the target proof inside the group,
so neither the counts nor their fixed hold establish a speed advantage or net
memory benefit. Candidate counts include other file READs and revisits. The
separate same-binary library curve cohort is required to measure the hint effect.

The first library cohort `.data/l1/` is retained as failed: target auditing
incorrectly equated `stat.st_dev` with `/proc/PID/maps`' device field. On this
Btrfs host the same library reports 54 through stat and 36 through maps, with
identical inode. Upstream Linux likewise uses a subvolume device for
[Btrfs stat](https://github.com/torvalds/linux/blob/v6.12/fs/btrfs/inode.c#L7363)
and the superblock device for
[maps](https://github.com/torvalds/linux/blob/v6.12/fs/proc/task_mmu.c).
The v2 observer now calibrates device/inode/kernel pathname from its own
untouched one-page mapping of the pinned canonical FD, then requires that exact
identity in the VM maps. It never falls back to inode-only matching or uses
privileged map_files/PFNs. Complete canonical bytes and before/after file-PTE
checks remain required. Valid partial ELF metadata tails are skipped, not padded
into invented full target pages. Both issues have retained failing regressions
(`library-evidence-before-fix.log`, `library-tail-before-fix.log`); all 32 Python
checks pass after repair. Two live DAX VMs then verify all 16,384 target pages
with calibrated identity in `.data/dax5d4/`. These diagnostics are separate from
performance samples; the failed cohort cannot supply aggregate results.

### Complete ordinary-rootfs library preflight

The retained six-variant projection of `.data/l2/` uses v2 target auditing and
`.data/pool-build-5/`. All 348 selected full-payload/COW/peer/exit checks,
actual data-path checks, resource limits and owned cleanup pass. Physical-hint
on/off and DAX each observe all 16,384 controlled target pages on common slots
across the four VMs after first readback. This remains N=1 per condition.

[Full idle curves](.data/l2/derived-current/memory-curves.svg),
[first five seconds](.data/l2/derived-current/early-memory-curves.svg),
[startup observations](.data/l2/derived-current/startup-memory-curves.svg),
[private-write curves](.data/l2/derived-current/post-write-curves.svg) and
[audited metrics](.data/l2/derived-current/summary.json) retain the six selected condition
identities. The historical raw cohort remains unchanged. Units below are complete-group MiB and VM-plus-pool CPU seconds over
the 60-second idle window; all cells are single observations.

| Condition | 2 s charged | 2 s PSS | 60 s charged | 60 s PSS | VM + pool CPU s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Firecracker block | 440.34 | 440.58 | 440.73 | 440.89 | 0.09 |
| pVisor rootfs off | 474.52 | 480.66 | 475.03 | 481.12 | 0.28 |
| pVisor rootfs KSM | 474.91 | 480.69 | 203.57 | 209.52 | 0.31 |
| Physical pool, hints off | 362.16 | 309.90 | 270.58 | 252.43 | 5.29 |
| Physical pool, hints on | 300.44 | 270.07 | 256.23 | 251.85 | 5.27 |
| Explicit immutable DAX | 227.97 | 297.63 | 228.43 | 298.09 | 0.42 |

Hints show earlier reclamation in this diagnostic; steady PSS and VM/pool CPU
are close. Charged-memory and PSS differences must stay separate: source cache
may be charged outside the group, and PSS omits kernel overhead. At 60 seconds,
hints-off has 87,209 pool VMAs and 19.17 MiB slab, versus 74,719 and 16.71 MiB
with hints; pool allocated blocks are 86.44 and 86.82 MiB. The larger charged
memory difference cannot all be assigned to canonical payload storage or net
host savings. Formal paired uncertainty remains unmeasured.

The DAX reference leaves ordinary anonymous RAM unpooled; the physical pool also
scans other guest/Python memory. Whole-group differences therefore do not isolate
library bytes alone. Global ksmd separately costs 0.52 CPU seconds for pVisor
KSM, outside quota. Full library-private writes leave +30-second charged
footprints of 699.36 / 733.18 / 466.86 / 539.19 / 524.73 / 486.73
MiB in table order. These writes change the library's application mapping; they
do not force eviction of clean file-cache pages or make all guest RAM unique.

The affected runtime store checks also pass (`just test pvisor -- --lib -E
"test(ram_backing)" --test-threads 2`: 44 tests, 476 outside the filter), including
physical slot ownership, IPC references and strict compression admission. Logs
are `.data/pool-check-5-runtime-stores.log`.

### Compression admission and read-sharing mechanisms

The physical pool performs no compression. In the complete q5 same-random cell
it still reaches 221.46 MiB, while sealed/local compression reach 377.31 / 372.09
MiB. The independent two-session 64 KiB random-block diagnostic retains zero
sealed objects after both healthy insufficient-savings rejections. This directly
identifies per-block compression admission as a limitation for incompressible
cross-VM duplicates; it is not a claim that the guest payload was compressed.

Readback distinguishes persistent sharing from eviction/refault as well. In q5's
repeated-content cell, sealed compression changes from 185.46 to 419.03 MiB on
full readback, local compression from 116.58 to 374.18, and physical sharing from
148.65 to 148.69. The corresponding whole-group verified read times are 1,113.22 /
1,155.44 / 833.75 ms; off is 645.33 ms. These are N=1 total read/validation costs,
not pure codec latency. The compressed pager restores private RAM; physical
MAP_PRIVATE reads retain canonical sharing. Compression mode also changes block
size, admission, IPC and restoration, so this matrix does not assign a percentage
of CPU time to the codec. Source-level codec and equality checks remain unchanged.

# Rust guest: immutable root and staged workspace reads

[B-FS-ENG](../REGISTRY.md#b-fs-eng) engineering A/B and separate
[B-FS-DIAG](../REGISTRY.md#b-fs-diag) request counts. Does keeping immutable
bytes across opens help a real command when both its executable and its input
are on overlays? Do not use these numbers for mutable bind mounts.

## Experiment

AMD Ryzen 7 9700X, x86 KVM, host affinity 2–5, fresh 2-vCPU/256-MiB VM per
arm. The two pVisor arms use the **same** Rust guest and debug VM runner.
`guest_metadata_case ROOT - PROBE rust-lower-data FIXTURE rg3` uses an immutable
executable root and a separate workspace (`Immutable` lower, fresh upper/work,
checkpoint durability, preimages and a separate apply target). Only
`PVISOR_VM_FS_LOWER_DATA_CACHE` changes, `0` versus `1`. The reference is
Firecracker 1.13.1, Linux 6.1.141, private ext4 over virtio-blk (writeback).
It is not an overlay and does not record a reviewable stage.

The payload runs unmodified `rg` 15.2.0 three times in the same VM:

```
LC_ALL=C rg --no-config --no-ignore --threads 1 --color never --sort path -l 'inode|Inode' tree
```

`tree` is `crates/pvisor-vm/src` at `39ad4188` (376 files); the payload checks
exit status and compares all 39 matching paths byte-for-byte each time.
Guest monotonic time from immediately before fork through output drain and
wait includes exec, not boot, fixture copy, host profiling or oracle comparison.
Fresh guest caches do not imply a cold host. One warm-up block, then 18 blocks
with shuffled arm order (seed 612). Guest image SHA-256
`0ea252524a2682bf4f2659af09ada225e05eb4964aa81e2bb82fa0dc7781c0af`;
same `rg` bytes on all arms
(`65edd8d2caf280b75922c6b572568305fc1991c4a809268b8e79b774d50ca2cc`).

## Result

Medians in milliseconds. Each row is computed per VM before aggregation.

| Guest clock | Rust off | Rust on | Firecracker |
|---|---:|---:|---:|
| First search | 226.84 | 192.91 | 45.11 |
| Searches two and three combined | 167.80 | 112.67 | 4.48 |
| All three | 396.59 | 303.77 | 49.52 |

Median paired reduction for all three: 90.00 ms (percentile bootstrap 95%
interval 85.22–105.37 ms, 10,000 resamples of paired blocks). This is a
single-host measurement, not a tail-latency or full Agent task result.

| Block | Rust off | Rust on | Firecracker |
|---:|---:|---:|---:|
| 0 | 421.060 | 315.194 | 52.205 |
| 1 | 391.291 | 300.978 | 50.538 |
| 2 | 376.569 | 286.888 | 49.264 |
| 3 | 377.749 | 306.850 | 49.469 |
| 4 | 408.153 | 302.107 | 49.389 |
| 5 | 393.391 | 291.465 | 50.743 |
| 6 | 395.538 | 328.705 | 49.191 |
| 7 | 470.154 | 328.549 | 51.152 |
| 8 | 412.216 | 329.367 | 47.773 |
| 9 | 378.173 | 301.621 | 48.994 |
| 10 | 409.455 | 320.338 | 50.283 |
| 11 | 423.976 | 302.884 | 51.718 |
| 12 | 397.640 | 309.960 | 50.317 |
| 13 | 378.389 | 292.104 | 50.817 |
| 14 | 388.374 | 304.210 | 47.944 |
| 15 | 388.591 | 275.164 | 46.251 |
| 16 | 410.584 | 318.831 | 48.367 |
| 17 | 408.707 | 303.339 | 49.576 |

## Separate protocol diagnostics

In fresh profiled runs of one versus three searches, **workspace** `READ`
counts were 614 → 1,842 off and 614 → 614 on. In both arms `OPEN` was
376 → 1,128, `READDIR` 164 → 492, and `GETATTR` 83 → 247. No repeated
workspace data fetch remains in this fixture; directory traversal and open
still cross to the host. Independently the executable **root** used 1,827
versus 588 `READ`s for one search (off/on), including bootstrap; immutable
file faults can fetch adjacent missing pages within the existing fault window.

An earlier prototype charged the full logical length of every file, even if
only a few executable pages faulted. It removed repeated workspace reads but
made the three-search total *worse* on immutable root + stage. The retained
implementation charges resident pages after the last open or mapping closes.
The [KVM contract runner](../../pvisor-kernel/tests/immutable_lower.py)
tests both reuse and eviction, including sparse files, and keeps the
copy-up, mapping and selective apply/drop checks separate from timing.

## Comparison to an exclusively owned directory

Same Rust image, host, CPU affinity and 2-vCPU/256-MiB guest size as above.
Compare ordinary passthrough (TTL zero), exclusive passthrough (explicit
1-second TTL), and the immutable root + staged workspace with lower data
objects enabled. No concurrent host writes. Ten randomized blocks per
workload (seed 61721), one fresh VM per arm and workload, profiling off.
The four [disk probes](../vm/probe.c) create their own writable file and
verify the full 4 MiB readback. In the staged arm `/work` resolves to
`/workspace`, **not** the executable root. These reads exercise the **upper**,
not an immutable lower receipt. The `rg` payload and output oracle are as above.

| Guest clock (median ms) | Live passthrough | Exclusive passthrough | Staged | Paired staged/exclusive, 95% bootstrap interval |
|---|---:|---:|---:|---:|
| 4 KiB sequential hot reads | 26.511 | 0.428 | 0.419 | 0.97× [0.80, 1.16] |
| 4 KiB shuffled hot reads | 26.369 | 0.452 | 0.462 | 1.02× [0.93, 1.13] |
| New sequential writes + fsync | 34.585 | 34.442 | 35.536 | 1.05× [1.00, 1.07] |
| Shuffled overwrites + fsync | 35.922 | 34.240 | 35.740 | 1.03× [0.95, 1.07] |
| Three `rg` searches | 1006.643 | 407.605 | 307.744 | 0.75× [0.71, 0.83] |

In a separate profiled VM, each staged hot-read probe incurred **4 `GETATTR`s**
and **1,024 `READ`s** on the workspace for the whole task including warm-up.
The writes and `fsync` went through to the host; these are not tmpfs timings.
This is a comparison of complete attachment policies, not a TTL-only A/B or
a claim that the live passthrough is now faster. It excludes preparing a
baseline, VM boot, and later stage review/apply.

## Automatic reuse in a mixed workspace

No prepared rootfs or workspace catalog. The executable root is ordinary
zero-TTL passthrough. The workspace is a writable stage over, in priority
order, a mutable overrides directory, a physical immutable layer and the
live project (also the apply target). Of the same 376 files sorted by relative
path, every fourth file is in the live project; the other 282 are in the
immutable layer. No data lives in the overrides directory during timing.
Every search checks the same 39 matching paths. Root passthrough, layer order,
journal and guest (`63bcd707f903750f09ed4f75368f4ca813ab0fc88802fffd00643ff3ae0fe48a`)
are identical between the disabled and automatic arms.

Sixteen randomly ordered blocks (seed 1762), fresh 2-vCPU/256-MiB VM per arm,
same host and affinity as above. The exclusive reference serves a separate
complete copy of the tree and executable root with a one-second TTL and has
no stage. Times exclude fixture construction and VM boot.

| Guest clock (median ms) | Mixed, disabled | Mixed, automatic | Exclusive passthrough |
|---|---:|---:|---:|
| First search | 278.96 | 292.33 | 171.18 |
| Second and third combined | 309.86 | 276.49 | 235.84 |
| Three combined | 589.15 | 567.28 | 406.53 |

The paired reduction for the two repeated searches was 26.12 ms (95% bootstrap
interval 17.30–30.55 ms). For the full three-search command the paired reduction
was 6.28 ms (−5.17 to 26.10 ms): no resolved whole-command speedup.
This mixed live view did **not** match the exclusive reference.

Separate profiled runs: with the policy disabled, one versus three searches
issued 614 versus 1,842 workspace `READ`s. With the default policy they issued
614 versus 920: the repeated-search delta fell from 1,228 to 306. Each pass
received 282 immutable receipts for 376 host `OPEN`s and zero `READINDEX`s.
Over three passes `GETATTR` increased from 247 to 529: a non-retaining fallback
open refreshes its backing attributes rather than using a previous layer's EOF.
The [mixed KVM regression](../../pvisor-kernel/tests/immutable_lower.c)
publishes a four-byte mutable shadow while an 8,209-byte lower is open;
after copy-up and unlink, the original handle must still read the original bytes.

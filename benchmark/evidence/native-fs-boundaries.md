# Rust native filesystem: fewer crossings or cheaper crossings?

Engineering A/B ([B-FS-ENG and B-VM-GUEST-ENG](../REGISTRY.md)), not the
three-backend gate. These are incremental experiments against PR #213 at
`0249c8ec`, **not** speedups over main. All VM cohorts below used real x86_64
KVM, one guest vCPU and 256 MiB on a Ryzen 7 9700X, btrfs,
Linux `7.3.3-300.fc45.x86_64`. No cache drops or outlier removal; some
experiments ran concurrently on different physical cores of the same host.
Do not add improvements from different cohorts.

## Change where the work happens

**Read inputs.** The [fixed-input experiment](../filesystem/read-input.md)
starts from 128 existing files (4 MiB). Deliver them as an ordinary read-only
directory, or serialize them into one file and import it into guest tmpfs.
Both arms run the same POSIX consumer. Time from before any pack construction
or VM launch until the host receives the correct answer. Completion of a
separate full-tree audit and normal exit are required for acceptance, but
not timed. This is **not** a comparison against Image Pack V1 or an overlay
admitted to the immutable directory cache.

| Workload | Direct / packed median ms | Paired packed − direct, ms [95% interval] |
|---|---:|---:|
| Open 4 of 128 files once | 90.55 / 113.63 | +21.93 [+11.30, +24.08] |
| Enumerate, stat and read all files four times | 261.01 / 150.63 | −100.60 [−145.67, −82.43] |

Pack build took ~1.5 ms and guest import ~23 ms. The repeated scan recovered
that cost; the sparse lookup didn't. Fifteen randomized complete four-cell
blocks (seed 21342), 60 successful measured VMs, four separate smoke VMs;
paired block bootstrap, 20,000 draws. Invalid length/mode and corruption
of an unread file were tested separately. The guest copy is private, **not
a read-only mount**: this fixed trusted reader never mutates it. No memory
density, live host coherence or general filesystem contract was tested.

**Write outputs.** The [publication experiment](../vm/tree-granularity.md)
fixes the output at eight new files (4 MiB), varies the producer's write
size and keeps the bulk export size at 256 KiB. Time from private trial setup
through VM exit and completion of the published directory, including syncing
each destination file and its directory. All names, bytes, modes and UID/GID
were independently checked on the host.

| Producer write | Shared / guest-local then export, median ms | Paired local − shared, ms [95% interval] |
|---|---:|---:|
| 1 KiB | 236.91 / 116.37 | −116.14 [−126.01, −101.92] |
| 16 KiB | 111.75 / 115.40 | +4.84 [−13.09, +13.80] |
| 256 KiB | 110.51 / 115.99 | +6.97 [−1.51, +13.01] |

Fifteen randomized complete blocks (90 measured VMs, six warm-up VMs,
seed 21316). At 256 KiB, the guest publication phase alone was **slower**
by 6.66 ms [6.04, 7.48]; the end-to-end difference was unresolved. Injecting
a conflict during export left the conflicting inode and its bytes unchanged
and `ready` absent: this is explicit publication of a new tree, not
transparent writeback, apply or host power-loss recovery.

Both experiments used the same `vm_api_case` (`4eb864bf534a`); they are
prototypes, **not product defaults**. Local records:
`benchmark/filesystem/.data/read-input-213/` and
`benchmark/vm/.data/granularity.HnZz1K/`.

## Keep the filesystem contract and reduce the work per request

**Directory enumeration.** The host now passes one bounded slice directly
to the descriptor callback when the requested bytes fit, instead of
allocating a vector. Transfers crossing descriptors still deliver all
slices to one callback. The [host READDIR test](../../crates/pvisor-vm/src/devices/virtio/fs/server.rs)
on a warm 100-file tmpfs directory took 7.86 → 5.84 µs/request with one
output descriptor, 8.21 → 6.33 µs/request with 257-byte fragments (eight
alternating pairs, 10,000 requests per shape after 100 warm-ups, CPU 7,
test-profile `pvisor-vm` at `opt-level=3`). This excludes guest/queue time.

The [unchanged guest `getdents64` probe](../../pvisor-kernel/tests/readdir.rs)
independently verified the names of 8,192 entries, short buffers, EOF,
rewind and cookie resume. Same guest kernel, same read-only workload on
fresh mutable roots, handoff off; only the descriptor change differs
between the two VMMs (`431d196ef021` / `b90ac44d7e40`). Fifteen fresh VM
pairs on CPUs 2–3, seed 21331. The four buffer sizes are correlated scans
in one VM, not four independent repetitions. Times include guest decoding:

| Application buffer (bytes) | Before / after median ms | Median paired time change [95% interval] |
|---:|---:|---:|
| 4,096 | 4.36 / 4.17 | −3.62% [−7.00%, −1.15%] |
| 8,191 | 2.70 / 2.59 | −6.30% [−8.17%, −2.18%] |
| 32,768 | 1.80 / 1.63 | −5.88% [−9.57%, +7.16%] |
| 65,536 | 1.64 / 1.53 | −7.00% [−10.88%, +3.91%] |

No resolved difference at the two large sizes. Intervals are paired
percentile bootstrap, 20,000 draws, without multiple-comparison correction.
Use the [existing fixture recipe](../../pvisor-kernel/README.md#tests-and-hardware-acceptance)
with a runner per version. Local batch: `benchmark/vm/.data/descriptor-readdir-213/`.

**Many open descriptions for one inode.** Page-cache I/O can use the newest
live handle with the appropriate access mode. Avoid upgrading every `Weak`
only to drop it again: prune first, then search newest-first and stop at a
match. Pruning is still linear. A read-side-only ablation (`4eb864bf534a` /
`1d5e706dbf46`) timed [1,024 shuffled cold-page reads](../../pvisor-kernel/tests/read_handles.rs);
the host disk cache was not cold. Fifteen measured pairs per handle count
on CPUs 0–1 after two warm-up pairs, seed 21317; 102/102 VMs passed.

| Independently opened readers | Before / after median ms | Median paired time change [95% interval] |
|---:|---:|---:|
| 1 | 34.93 / 35.48 | +0.65% [−0.01%, +1.68%] |
| 32 | 35.75 / 35.31 | −1.14% [−2.92%, +0.69%] |
| 256 | 36.65 / 34.89 | −4.77% [−5.83%, −4.21%] |

The separate 256-reader diagnostics recorded exactly 1,407 `READ`, 1,608
`GETATTR` and 262 `OPEN` in both arms. This isn't a cache-policy change.
The integrated code uses one selection rule for both access modes; the
table does **not** establish faster writes. Local records:
`benchmark/vm/.data/reader-fanout-213/`.

Repeating the same 102-VM protocol against the **integrated** VMM/kernel
gave +0.35% [−0.10%, +0.93%] at one reader, +0.14% [−0.36%, +0.74%]
at 32, and **−4.48% [−5.48%, −4.12%]** at 256 (36.42 → 34.79 ms).
An independent KVM check retained a mapped inode, a newer write-only handle
and an even newer read-only handle, synchronized writes before and after
closing the newer writer, and verified final bytes on the host. Live
rewrite/truncate/replacement and short-write/`EFBIG` checks also passed.

## Does this speed up a complete tool invocation?

**No resolved improvement on a fresh-VM repository search.** On CPUs 0–1,
30 randomized balanced pairs ran the unmodified Fedora `rg` 15.2.0 inside
the Rust guest. Baseline (`4eb864bf534a`) and integrated default VMM/kernel
(`668c2ebb1f0e` / kernel `3d109da3dab4`) took 345.33 / 336.15 ms from
runner invocation through checked guest exit. The median **within-pair**
change was **+0.60% [−0.70%, +2.02%]**, not the ratio of arm medians.
This was ordinary virtio-fs: neither guest-local prototype nor opt-in
handoff was enabled.

To reproduce, export `crates/pvisor-vm/src` at `0249c8ec` as the input
(`git archive ... | tar -x`); copy `/usr/bin/rg` and its dynamic loader and
libraries (`ldd`) into fresh writable roots. Pass that input as `--readonly`
to each `vm_api_case`. Use `--cpus 1 --memory 256 --task-stdio -- /usr/bin/rg
--no-config --no-ignore --threads 1 --color never --sort path -l 'inode|Inode'
/readonly`. Preparation isn't timed. Every VM exited zero with empty stderr
and exactly the same 40 paths as native `rg`; all 376 input files retained
their SHA-256. No SHA or oracle work ran inside the clock. Seed 21339 for
paired order and bootstrap; every pair retained. Local raw commands, input
hashes and outputs: `benchmark/vm/.data/rg-default-213/`. This tests one tool
on a directory share, not a full CLI Session, build, stage seal or apply.

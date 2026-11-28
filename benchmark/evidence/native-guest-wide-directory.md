# Native guest: directory-cache maintenance under insertion pressure

Engineering A/B and protocol diagnostics ([B-FS-ENG](../REGISTRY.md#b-fs-eng),
[B-FS-DIAG](../REGISTRY.md#b-fs-diag)). Does adding a new name still perform
`LOOKUP` on unrelated siblings in a large mutable directory?

## Protocol and correctness

Two fresh x86_64/KVM VMs per kernel, 2 vCPUs / 128 MiB, writable passthrough
virtio-fs, named task streams, no READINDEX. One VM stats 1,024 existing
names and 1,024 missing names; the other uses 4,096 of each. Both perform
the same [rename, open-fd, watch and mount checks](../../pvisor-kernel/tests/stat_visibility.c).
The counts are final host protocol records for the whole VM, including PID 1.

| Names of each kind | Previous LOOKUP | Candidate LOOKUP | Previous FORGET | Candidate FORGET |
|---:|---:|---:|---:|---:|
| 1,024 | 2,113 | 2,113 | 0 | 0 |
| 4,096 | 17,473 | 8,257 | 0 | 4,096 |

In the candidate, 6,144 extra user queries account for exactly 6,144 extra
`LOOKUP`s. The baseline added another 9,216 (2,048 + 3,072 + 4,096).
Reclamation is not free: evicted inodes release FUSE lookup references with
one-way `FORGET`. `GETATTR` counts were unchanged (2,136 and 8,280).
The CI check compares the growth in `LOOKUP`, not elapsed time or an exact
schedule of asynchronous releases.

## Guest timings (profiling disabled)

Ryzen 7 9700X, Linux 7.3.3-300.fc45, host affinity 2–5, 2 vCPU / 128 MiB.
Each row's workload ran in 12 shuffled pairs of fresh VMs, with no samples
excluded (order seed 72185). The wide probe times only the first pass of
relative `newfstatat` calls on existing files; fixture setup, the negative
pass and semantic checks are outside that clock. The separate existing
[M01/M02 worker](../filesystem/workloads_v2_guest.rs) checks 2,048 full paths
per round, three resident rounds, with an independent host `--verify` after
each VM. Brackets are 95% percentile bootstrap intervals for the median
within-pair difference (10,000 resamples, seed 35671).

| Case | Previous median (ms) | Candidate median (ms) | Paired candidate − previous (ms) |
|---|---:|---:|---:|
| 256 existing names, first pass | 57.7 | 49.0 | −11.5 [−24.2, +16.1] |
| 4,096 existing names, first pass | 1,389.9 | 641.5 | −686.0 [−945.5, −468.0] |
| M01 existing names, first round | 599.8 | 514.4 | −66.2 [−132.7, −12.7] |
| M01 existing names, rounds 2+3 | 912.0 | 1,026.4 | +81.5 [+25.6, +196.9] |
| M01 existing names, all three rounds | 1,530.4 | 1,559.9 | +53.3 [−147.8, +165.3] |
| M02 missing names, all three rounds | 1,729.1 | 1,671.0 | −11.5 [−193.4, +36.5] |

The first wide pass improved. The two repeat passes of M01 regressed:
the baseline keeps valid siblings, whereas the candidate can release and
later rebuild expired, unused entries. The combined three-round interval
does not resolve a change. Do not infer an overall Git/build speedup,
peak memory reduction or a tail percentile from these single-host trials.

## Scope and reproduction

- Baseline is the preceding AP/headless/DMA build (`84a6017cc104`), not an
  older Linux or Rust kernel. Candidate kernel SHA-256 starts `cf1f9cbdf88d`.
  Both ran with the same debug VM runner sources and release guest kernel.
- Use the [metadata acceptance command](../../pvisor-kernel/README.md#kvm-metadata-acceptance).
  It also checks host mutations, staged copy-up and non-root directory search,
  including seeded concurrent runs. The cache target is local to a directory
  and is not a global memory bound. This change does not remove locks around
  lookup or change mount coherence.
- Local protocol records, command lines and unprofiled timing samples are
  under `target/dentry-ab/`. The cache-budget ktest passed with its OSDK
  ELF entered at `__linux64_boot` on Firecracker/KVM; the OSDK/QEMU launcher
  did not produce a test verdict. ARM64 cross-built; not run on ARM hardware.

## Follow-up: repeated scans on `9f251505`

Does the bounded cache still rebuild the same files on every pass? This
comparison starts **after** the rebase. “Before” includes the AP, headless/DMA
and bounded directory-cache changes. A separate control reverts just the
directory-cache changes to restore the old whole-directory sweep. Results
above are historical, not samples from this comparison.

**Method.** Fresh x86_64/KVM instances, writable passthrough roots, 2 vCPUs,
128 MiB, three consecutive passes per VM, host affinity 2–5. The existing
M01 worker checks 2,048 full paths and has its output verified on the host.
A second probe uses relative `symlink_metadata` on 8-byte files and reads
guest `MemFree` outside each pass's clock. Git uses a real repository with
2,048 tracked files in one directory and one known dirty file; in each pass
a new, unmodified `/usr/bin/git` runs `status --porcelain=v1 -uno` in the
same VM. Optional index writes and index preloading are disabled. All three
outputs must name only the dirty file; host Git checks it again at the end.
This is not the cost of showing untracked files or a build.

**Why both changes?** With the bounded round-robin cache, sequential misses
replace exactly the entries that the next pass needs. A batch-only experiment
reduced the number of one-way requests from 5,120 singles to 42 batches in
a three-pass protocol run. M01 rounds 2+3 improved by a paired median
25.7 ms relative to the bounded baseline (15 pairs, 95% interval 8.1–39.5 ms),
but were still 19.9 ms slower than the sweep control in a separate 15-pair
comparison. Batching alone does not change the number of inode constructions.

The retained cache instead repeatedly replaces the newest arrival in one
probationary slot, periodically rotating that slot through the resident
entries. It does not mark every hit, increase the target or extend metadata
validity. A tested alternative gave recently hit entries a second chance,
but consumed about 0.9 MiB more free guest memory from pass 1 to pass 3 on
the relative-path fixture; it is not in the product. Even with the retained
policy a changed working set may take several passes to displace old names.

**Mechanism, separate from timing.** Diagnostic-only guest counters at
inode construction, destruction and forget-work submission were read at
`syncfs` before and after each round, with 600 ms for asynchronous work.
The diagnostic kernels were built from the bounded baseline and the retained
replacement/batching prototype (before the final queue helper refactor).
The same guest queried 2,048 relative names per round. These are counts in
one resident VM per arm, not three fresh VM totals subtracted from one another.

| Pass | Inodes constructed, before → after | Inodes dropped, before → after | Forget work items, before → after |
|---|---:|---:|---:|
| 1 | 2,048 → 2,048 | 1,024 → 1,024 | 1,024 → 9 |
| 2 | 2,048 → 1,026 | 2,048 → 1,026 | 2,048 → 12 |
| 3 | 2,048 → 1,027 | 2,048 → 1,027 | 2,048 → 11 |

Host final protocol counters: 5,120 single forgets before; 1 single plus
31 batch forgets after. Both issued 6,194 `LOOKUP`s and 6,212 `GETATTR`s:
retaining an inode does not make its expired attributes valid. Batching
depends on worker scheduling; 32 is not a fixed message budget. For the
tracked-only Git probe, both arms constructed and dropped **zero** inodes
in rounds 2 and 3; there was no repeat-pass reclaim work to remove.

**Timings.** Profiling and diagnostic counters off. Each case below uses
20 randomized pairs (order seed 6101), no discarded samples. Arm columns
are medians; the last column is the median *within-pair* difference and
its percentile bootstrap 95% interval (10,000 resamples, seed 1925).
Rounds in one VM are correlated, not independent samples. This was a
shared host, not a dedicated CPU or cgroup-isolated performance gate.

| Workload / clock | Before (ms) | After (ms) | Paired change (ms) |
|---|---:|---:|---:|
| M01, first pass | 479.1 | 473.7 | −8.2 [−46.5, +21.2] |
| M01, passes 2+3 | 938.5 | 888.4 | −82.2 [−99.0, −34.2] |
| M01, all three | 1,395.4 | 1,375.1 | −75.6 [−108.6, −46.3] |
| Relative paths, 2,048 names, passes 2+3 | 303.5 | 289.8 | −17.4 [−32.5, −9.7] |
| Relative paths, 4,096 names, first pass | 334.8 | 318.8 | −16.6 [−24.6, −8.0] |
| Relative paths, 4,096 names, passes 2+3 | 644.6 | 578.7 | −59.6 [−73.0, −38.3] |
| Relative paths, 256 names, passes 2+3 | 31.7 | 33.0 | +1.1 [+0.4, +2.0] |
| Tracked-only Git, passes 2+3 | 1,950.3 | 1,984.5 | +7.1 [−22.8, +86.4] |

Against the **whole-directory sweep** control in 20 separate pairs, M01
first pass was 483.6 → 419.1 ms, paired −70.6 [−88.3, −62.7]; passes 2+3
were 784.3 → 781.5 ms, paired +5.9 [−11.6, +15.3]. The old repeat-pass
regression was not detected in this cohort; this does not establish
equivalence. The three-pass total improved by 61.6 ms [50.2, 87.8].

The small-directory clock is slightly slower in this batch; it does not
exercise reclaim. For 2,048 names, paired guest `MemFree` was 120 KiB higher
after the first pass and 464 KiB higher [294, 484] after the third. For 256
names both differences were zero. These are free pages at observation time,
not peak guest allocations or host PSS.

**Git with its normal index and untracked-file behavior.** A separate
15-pair check on host CPUs 0,1,6,7 ran `git status --porcelain=v1` with
default preloading and index refresh, and one additional untracked file.
Both the modified tracked file and untracked file had to appear on every
run. First-pass medians were 1,233.0 → 1,208.6 ms (paired −2.0 ms
[−52.9, +7.4]); passes 2+3 were 460.6 → 444.0 ms (paired −20.1 ms
[−27.2, −3.8]). On the first pass the diagnostic guest submitted 1,875
forget jobs before and 21 after. On each repeat pass **both submitted only
one job**. The observed repeat timing difference is not direct evidence
that faster cache reclamation speeds up warm Git in general.

**Verification and artifacts.** The [KVM metadata suite](../../pvisor-kernel/tests/fs_metadata.py)
still saw 2,113 → 8,257 `LOOKUP`s when scaling from 1,024 to 4,096 names of
each kind. Host-mutation visibility, open paths, directory watches, mount
identity, permissions and three staged concurrency seeds passed. In a
separate 4-vCPU / four-worker VM, four guest threads repeatedly traversed
four directories hard-linked to the same 2,048 files. Held fds and sampled
reads remained valid; the host verified all 8,192 names, link counts and bytes.
Three Firecracker/KVM ktests cover the two-probe budget and pins, retention
and eventual rotation, and ownership of full batches and a partial tail
including exact 64-bit reference counts for repeated node IDs. ARM64 was
cross-built only.

Working data: `target/repeat-dir/{paired-final,control-final}/rows.json`,
`default-status/checked-default/rows.json`, per-arm commands and outputs,
`instrument.py`, `diag-{base,trial}-wide/err`, `round.rs`, `pairs.py` and
`identities.json` under the same directory. Embedded kernel bytes verified
against the prepared bundles: before `a20417ed178c`, after `528f49442ed8`,
sweep control `c7e29e1371b5`. For the persistent benchmark, use the
[M01 worker](../filesystem/workloads_v2_guest.rs) with `M01 /work /input 3 128`
and the host `--verify` pass against the same fixture. Do not mix these
single-host results with the cross-backend performance gate.

# PR #219 guest regression retest

Engineering diagnostic for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
Which red cells reproduce under paired measurement, and does the Rust
native warm-read difference depend on the virtio-fs transport path?

## Rust native warm reads

The [first PR CI run](https://github.com/DeepLink-org/pvisor/actions/runs/46090489008)
compared `530fba64` with the CI merge of `5b9dab34`. Both Rust native warm
reads exceeded the 15% threshold. Reanalysis of the archived 16-pair runs on
that CI host found +33.40% [26.06%, 131.59%] for sequential read and +29.08%
[−17.78%, 39.72%] for shuffled read. The sequential read regression was
reproducible there; the shuffled-read interval is wide.

The local retest used the verified CI executables and a separate pair of
locally built executables (`530fba64` and `b4212df0`). It used the unchanged
gate probe, rebuilt locally, on Linux x86_64/KVM, Ryzen 7 9700X, btrfs,
host CPUs `2-5`, fresh 1-vCPU/256 MiB guests, and 20 randomized blocks per
workload. Both idle handoff and wall deadlines were off. Within each block,
both revisions ran with default virtio-fs queue handoff (`auto`) and with it
disabled (`off`). The two pairs of executables are separate comparisons;
these measurements do not identify a change from the older candidate to
the current commit.

| Executables | Warm read workload | Queue handoff | Main → candidate medians (ms) | Median paired change, 95% interval |
| --- | --- | --- | ---: | ---: |
| Old CI | Sequential | Auto | 5.891 → 6.639 | +9.68% [+1.80%, +20.56%] |
| Old CI | Shuffled | Auto | 5.779 → 6.517 | +10.97% [+3.86%, +26.86%] |
| Old CI | Sequential | Off | 13.143 → 13.213 | +1.12% [−1.24%, +2.84%] |
| Old CI | Shuffled | Off | 12.929 → 13.199 | +1.60% [−0.44%, +2.30%] |
| Local current | Sequential | Auto | 6.757 → 5.585 | −8.31% [−20.01%, −4.85%] |
| Local current | Shuffled | Auto | 5.928 → 6.406 | +1.02% [−8.97%, +14.59%] |
| Local current | Sequential | Off | 13.075 → 13.217 | +2.74% [+0.97%, +4.28%] |
| Local current | Shuffled | Off | 12.959 → 13.101 | +1.63% [+0.37%, +3.73%] |

Intervals use 10,000 bootstrap resamples of whole blocks. The old CI
executables show a smaller slowdown locally when using `auto`, not when
using `off`. This narrows where to look, but does not identify a changed
polling branch or prove a fix in `b4212df0`. Disabling queue handoff is not
a viable performance fix: it makes both versions slower in absolute time.
Separate, untimed profile boots confirmed `auto` negotiated on all four
executables and `off` produced no worker handoff hits. These counters
include startup and validation and do not attribute the timed slowdown.

## Red cells from the local gate

The 13 red cells from the preceding [local gate](native-wall-deadline.md#guest-regression-gate)
ran 20 balanced, randomized `530fba64`/`b4212df0` pairs each with the normal
transport. None of the four Rust native cells reproduced a median slowdown
above 15%. Sequential write + `fsync` was −1.45% [−11.93%, +3.18%] and
shuffled overwrite + `fsync` was +4.59% [−10.63%, +12.35%]. Linux
`tmpfs-shuffled-read` was +15.92% [−13.20%, +44.64%], the only median above
15%; the interval does not establish a revision effect.

In a separate A/A, the same `b4212df0` binary and firmware ran the 15
previously red cells in two sequential cohorts of 12 fresh VMs per cell.
Linux tmpfs shuffled read and overwrite moved +24.51% and +16.78% between
cohorts despite identical code. This was a targeted A/A using raw
per-VM medians, not a complete Criterion gate. All 1,200 timed VM runs in
the three diagnostics completed successfully; none were excluded.

## CI on `b4212df0`

The [second CI run](https://github.com/DeepLink-org/pvisor/actions/runs/46096246036/job/140833872105)
failed three default-mode Rust native cells. Its candidate was the CI merge
of `b4212df0` into `530fba64`. The same CI runner also completed 16
alternating pairs per red cell. The threshold and original verdict remain
unchanged.

| Workload | Standard gate | Same-CI paired median change, 95% interval |
| --- | ---: | ---: |
| Sequential write + `fsync` | +174.94% | −1.65% [−62.83%, +42.51%] |
| Sequential warm read | +111.63% | +12.57% [+6.46%, +23.70%] |
| Shuffled overwrite + `fsync` | +51.03% | +3.43% [−23.22%, +7.86%] |

Warm read is the remaining reproducible slowdown on that runner. In the
standard gate, five of ten candidate warm-read samples took 15 ms or more;
the other five took 5.44–5.75 ms. No samples were discarded. The write and
overwrite median slowdowns did not recur, but the intervals do not all
exclude a 15% effect.

### Replaying the CI executables on local KVM

The downloaded runner SHA-256 values match the CI manifests. The same gate
probe source was rebuilt locally. Two artifact sets were tested separately:
the CI pair above, and locally built `530fba64`/`5448701e`. Each of the three
red workloads used 16 randomized blocks on one Ryzen 7 9700X host (Linux
`7.3.3-300.fc45.x86_64`, btrfs instead of the CI backing filesystem, host
CPUs `2-5`, fresh 1-vCPU/256 MiB VM). Neither experimental timer mode nor
idle handoff was enabled. Changes are paired medians with 95% bootstrap
intervals (10,000 resamples).

| Workload | CI executables, `auto` | CI executables, `off` | Local `5448701e` vs `530fba64`, `auto` |
| --- | ---: | ---: | ---: |
| Sequential write + `fsync` | −5.79% [−16.35%, +4.51%] | +1.78% [−5.35%, +12.38%] | +3.18% [−2.38%, +11.80%] |
| Sequential warm read | −5.01% [−11.87%, +17.61%] | +1.00% [−0.24%, +2.66%] | +10.38% [−6.82%, +17.87%] |
| Shuffled overwrite + `fsync` | +2.08% [−10.28%, +13.68%] | −0.11% [−5.49%, +8.45%] | +9.45% [−10.89%, +41.40%] |

All 640 timing trials and 24 separate profile runs completed; none were
excluded. None of the severe median slowdowns reproduced locally. For the downloaded
candidate, forcing queue handoff off slowed warm read from 5.40 ms to
12.89 ms (arm medians), so forcing it off would discard a useful fast path.
The separate phase probe retained the gate's write and verification logic
and took one more timestamp just before the final `fsync`. With the old CI
executables and `auto`, the final `fsync` itself did not show a slowdown:
sequential −0.12% [−21.69%, +3.18%], shuffled −5.44% [−14.03%, +2.91%].
The sequential write-loop interval remains wide. Separate profile boots
confirmed that both old CI artifacts negotiated `auto` when requested.
Their counters include setup and verification, so neither the counts
nor the local phase measurements identify where time went in CI.

Raw CI artifacts are in `target/pr219-ci-recheck.B2KsQG/ci/`; the local
plans, executable digests, profiles, and measurements are in
`benchmark/vm/.data/ci-b4212-recheck.pu1UM3/`.

## Preview against the updated target branch

GitHub reported `5448701e` as conflicting with `main` at `344d4030`.
The only textual conflict is in `pvisor-kernel/README.md`; the target
branch also includes virtio-fs changes and two additional gate workloads.
A local integration preview used the source tree from `git merge-tree`
(`ca2fb442f2e2`); the conflicting README is not a kernel or VM runner
build input. The baseline and candidate were built locally from
`344d4030` and that preview, respectively.

On the same local KVM host, 20 randomized pairs per previously red cell
used the new main's gate probe, default timer and transport settings,
and no handoff lease. All 120 runs completed.

| Rust native workload | Main → preview medians (ms) | Median paired change, 95% interval |
| --- | ---: | ---: |
| Sequential write + `fsync` | 12.388 → 11.458 | −4.53% [−6.16%, +3.29%] |
| Sequential warm read | 5.982 → 5.261 | −7.87% [−21.07%, +0.91%] |
| Shuffled overwrite + `fsync` | 12.808 → 12.094 | −9.53% [−15.00%, −4.09%] |

The standard 51-cell matrix also completed for both cohorts using the
same previously prepared Linux firmware and pinned Firecracker reference.
**32/34 product checks passed; the preview gate failed.** All 17 Rust native
cells passed, including the three former red cells (+3.93%, −4.61%,
and −19.31% for sequential write, sequential warm read, and shuffled
overwrite, respectively). The two failed checks were Linux firmware
`memory-copy` (+21.99%) and `tmpfs-shuffled-overwrite` (+32.08%).
Their separate 16-pair follow-ups were +8.89% [−11.55%, +25.46%] and
+1.69% [−35.67%, +35.40%], respectively. Neither paired median repeated
the threshold crossing, but neither interval excludes a 15% slowdown.
This is evidence about the current combined code on the local host, not
about the older CI host or an attribution of the original failure.
Preview artifacts and results: `benchmark/vm/.data/pr219-preview.MmCIJf/`.

## Repair and validation

- Do not change the tick, TTLs, `fsync` semantics, or the 15% threshold to
  clear these results. The read tests validate guest-cached data on a shared
  root; the queue optimizes those metadata round trips. It is not a disk
  throughput test. The gate does not enable the new idle or deadline path.
- If the current CI build still reproduces the warm-read regression, compare
  both revisions on that host with the same CPU placement. Bracket the
  timed read loop and attribute its metadata requests to vCPU inline vs
  worker completion, with poll misses, IRQ/exit counts, and scheduling
  delay. A same-VMM guest-bundle swap can then separate guest changes from
  host binary/build effects. Change the polling/backoff only if an isolated
  change explains the measured loss and recovers it without breaking
  spaced reads or shared-file visibility.
- For short tmpfs cells and durable writes, calibrate on an exclusive
  KVM runner with a complete same-artifact A/A before enforcing the
  existing gate. Keep each workload and its threshold; if sequential
  runs still drift, validate a balanced per-workload schedule against A/A
  before changing the measurement protocol. Neither these diagnostics
  nor a new local pass can replace the old gate verdict.

Raw evidence is in `benchmark/vm/.data/regression-retest.1XOvf0/`.

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

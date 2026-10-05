# How long does applying reviewed changes take compared with Git patches?

## Main conclusions {#conclusions}

**Small changes fit interactive review: about 15 ms for ten files and 0.84 s for 1,000. Applying 100,000 files takes about 5.5 minutes, unsuitable for frequent large submissions. Git patch is substantially faster in the same comparison; pVisor adds preimage conflict checks, persistence and recovery.**

| Need | Selection implication |
|---|---|
| Interactive application of a few files | pVisor apply fits |
| Large text updates with a Git workflow | Git patches provide a performance reference |
| Concurrent host edits | Check conflict detection and interruption recovery |

## Motivation {#motivation}

Staging must eventually support safe submission: preserve concurrent host edits and recover interrupted commits, alongside acceptable performance.

## Experiment design {#interpretation}

Actual staged tasks overwrite existing text files. Lower content and upper count are verified before timing. Measurements cover only the apply/drop CLI, excluding stage generation and per-file validation. N=30/10/3 for 10/1,000/100,000 files. One warmup per action for smaller groups, none for 100,000. Three large stages are prepared concurrently; timed operations run sequentially. Conflict changes the first host file and requires refusal with all other targets unchanged.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

For 10 files (N=30), the second value is descriptive P95. The 1,000-file (N=10) and 100,000-file (N=3) rows show only P50. Recovery injections (N=3) also show medians only, without tail estimates.

Measured on 2026-10-04; configurations retain separate samples. P50 is the median.

| Files | Operation | N | P50 / P95 ms |
|---|---|---|---|
| 10 | apply | 30 | 15.01 / 16.75 |
| 10 | drop | 30 | 3.38 / 3.82 |
| 10 | conflict | 30 | 4.07 / 9.44 |
| 10 | copy | 30 | 5.07 / 10.81 |
| 10 | git-apply | 30 | 0.73 / 0.85 |
| 1000 | apply | 10 | 836.38 |
| 1000 | drop | 10 | 24.44 |
| 1000 | conflict | 10 | 42.90 |
| 1000 | copy | 10 | 15.84 |
| 1000 | git-apply | 10 | 13.61 |
| 100000 | apply | 3 | 330396.22 |
| 100000 | drop | 3 | 996.55 |
| 100000 | conflict | 3 | 247496.21 |
| 100000 | copy | 3 | 1214.85 |
| 100000 | git-apply | 3 | 1464.63 |

### Analysis

Small batches fit interactive review; 1,000 files approach a second and 100,000 cost much more than copying or Git patches. The three largest applies took about 325–339 seconds; N=3 cannot establish stable tail latency. Conflict refusal at 100,000 files also takes about 247 seconds, revealing expensive validation. These costs are published without hiding the optimization gap.

Copy writes into an empty directory. Git apply updates equivalent text files but lacks the pVisor stage-cleanup/preimage/ledger/recovery protocol. They are cost controls with different semantics. Small and large batches retain separate provenance.

The 100,000-file stage emitted `trace append rejected: event exceeds size limit`. Target/ledger/conflict checks passed, but complete filesystem audit coverage is not claimed.

### SIGKILL recovery

Inject SIGKILL at prepared, target_applied or committed, then rerun and verify targets and committed ledger. The table includes actual injection hits only. Two 1,000-file committed-window attempts missed and do not count as successful injections; reports retain them. Recovery latency depends on the work already completed at interruption.

| Files | Requested kill state | N | Durable state at death | Recovery P50/P95 ms |
|---|---|---|---|---|
| 1000 | prepared | 3 | prepared | 1587.60 |
| 1000 | target_applied | 3 | target_applied | 120.98 |
| 10000 | prepared | 3 | prepared | 8856.00 |
| 10000 | target_applied | 3 | target_applied | 354.28 |
| 10000 | committed | 3 | committed | 259.47 |

### What the Git patch baseline means {#baseline-meaning}

`git apply` on the same inputs is a familiar cost baseline: about 0.73 ms for 10 files and 13.61 ms for 1,000, versus 15 ms and 836 ms for pVisor apply. Absolute budgets matter more: tens of milliseconds for a small edit, seconds for a thousand-file merge, and minutes for a hundred thousand files.

Git patch, copying, and pVisor apply have different workflows. This compares the measured cost of the same text updates, without claiming identical transactions. Consider whether preimage checks, selective merging, and crash recovery are requirements of the workflow. Large batches currently carry a clear performance cost.

### Scope {#acceptance}

SIGKILL recovery does not measure power loss, filesystem corruption or lost disk writes. All file types, symlinks and metadata combinations are not covered. Large projects should budget by submission size; ten-file results do not extrapolate to a million files.

### Downloads and reproduction {#run}

[Derived table CSV](apply.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

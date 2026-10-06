# How long does applying reviewed changes take compared with Git patches?

## Main conclusions {#conclusions}

**With two CPUs on Linux, warm caches and small text files, pVisor's median apply time is 17 ms for 10 files, 0.93 s for 1,000 and 14.26 s for 10,000, all higher than Git patch application. At 100,000 files the distribution has two clusters: 40% of samples have a cluster median of 109.73 s, and 60% have a median of 196.87 s.** Small changes suit interactive application; large changes need substantial waiting time.

| Requirement | Selection guidance |
|---|---|
| Applying reviewed changes to a few files | The ten-file case takes milliseconds |
| Large text updates where waiting time matters most | Git patch application has a lower measured cost |
| Preimage checks, selective application and interruption recovery | pVisor provides these workflows, with a substantial cost for large batches |

## Motivation {#motivation}

Staged changes deliver their value when you apply them. Choosing a review workflow requires knowing how that cost grows with file count, and what happens if you edit host files during application or the process is interrupted.

## Experiment design {#interpretation}

The host is an AMD Ryzen 7 9700X running Linux 7.2.8-200.fc44 x86_64. Measured processes are pinned to CPUs 0 and 1, with no benchmark-specific host memory cap. Caches are warm and the page cache is not actively cleared. The workload updates small text files in one directory; staged changes and Git patches are generated before timing. Workspace preparation and subsequent verification are outside timing.

At 10, 1,000, 10,000 and 100,000 files, measure pVisor apply, Git apply, native copying, drop and rejection of a conflict introduced before application. Each condition has three warmups and 30 samples, totaling 600 formal timings; a fixed seed randomizes size and operation order within each round. Successful samples must pass complete target-content checks and applicable application-ledger checks. Failures are retained separately, and all valid slow samples are retained without exclusions based on duration.

Independent correctness checks include three actual SIGKILL hits at each of three durable states in a 10,000-file application, and three host-edit injections during application using the same binary. For the latter, pause after one file has changed while the file to be edited still has its original content, write and synchronize the external edit, then resume. These checks cover finite injection windows, not every concurrent schedule.

## Data and analysis {#results}

Measured on 2026-10-06. Units: ms; each cell is **P50 / reference P95, N=30**, with zero failures. P95 from 30 samples describes observations rather than a stable tail-latency commitment.

| Files | pVisor apply | Git apply | Native copy | drop | Pre-apply conflict rejection |
|---:|---:|---:|---:|---:|---:|
| 10 | 17.21 / 42.47 | 1.08 / 1.57 | 2.56 / 3.07 | 4.25 / 5.60 | 2.24 / 2.88 |
| 1,000 | 925.96 / 1,788.33 | 14.56 / 18.26 | 16.84 / 19.79 | 26.99 / 40.03 | 21.44 / 23.33 |
| 10,000 | 14,259.71 / 21,914.94 | 155.31 / 173.19 | 114.55 / 122.87 | 204.20 / 298.34 | 183.90 / 200.78 |
| 100,000 | Two clusters, see below | 1,639.51 / 1,861.16 | 1,109.58 / 1,253.46 | 858.57 / 929.70 | 1,290.37 / 1,354.08 |

Apply at 100,000 files is reported as two clusters. Units: s.

| Duration cluster | Samples | Share | Cluster median |
|---|---:|---:|---:|
| Lower duration | 12 | 40% | 109.73 |
| Higher duration | 18 | 60% | 196.87 |

Across all 30 samples the observed range is 107.42–205.33 s, with a reference P95 of 203.99 s. Budget for both clusters rather than using the faster cluster alone. Timing data does not establish the cause of the two clusters.

### What the Git patch baseline means {#baseline-meaning}

Median differences use 5,000 bootstrap resamples of paired rounds with a fixed seed. Units: ms; 30 pairs per row.

| Files | pVisor apply − Git apply | 95% confidence interval for the difference |
|---:|---:|---:|
| 10 | +16.13 | +15.79 to +18.63 |
| 1,000 | +911.41 | +895.05 to +1,017.69 |
| 10,000 | +14,104.40 | +11,179.77 to +19,418.50 |

All three scales show higher pVisor latency. The separated distribution at 100,000 files is not summarized by a single median difference. Git patches also check patch context, but do not provide the same preimage checks, durable application ledger and interruption-recovery workflow. This comparison estimates waiting time for the same text updates; it does not claim identical transaction semantics.

### Host edits during apply {#concurrent-conflicts}

The independent injection check uses 10,000 files and N=3, without pooling these checks with the timing samples above.

| Valid injections | Conflicts detected | Host edits preserved | Silent overwrites | Unknown results |
|---:|---:|---:|---:|---:|
| 3 | 3 | 3 | 0 | 0 |

All three windows explicitly reject the conflict and retain the host edit, Prepared ledger and complete upper. Some files have already been applied before the conflict, so this does not mean whole-batch rollback, and does not cover every race between the final check and rename.

### SIGKILL recovery

At 10,000 files, N=3 per requested state. All nine injections hit the requested durable state, and rerunning passes target and Committed-ledger checks, with no missed injection windows. Units: ms; only medians and observed ranges are reported.

| Interruption state | Recovery median | Minimum–maximum |
|---|---:|---:|
| prepared | 20,790.33 | 19,599.02–21,358.11 |
| target_applied | 386.91 | 350.96–475.45 |
| committed | 428.82 | 374.26–443.29 |

Recovery waiting time depends on the work completed before interruption. This checks process SIGKILL rather than power loss or storage corruption; three observations do not establish tail latency or a reliability guarantee.

### Scope {#acceptance}

The data covers warm caches and small text files in one directory. It does not cover large binaries, all file types, symlinks and metadata combinations, or macOS. Full content validation ran during measurement; retained commands and ledgers were independently audited, but cleaned target directories cannot be reread byte for byte. Finite conflict injections do not prove detection of every concurrent edit.

### Downloads and reproduction {#run}

[Scale timings CSV](apply.csv) · [Paired Git comparison CSV](apply-comparisons.csv) · [Recovery checks CSV](apply-recovery.csv) · [Concurrent conflicts CSV](apply-concurrent-conflicts.csv) · [Provenance CSV](apply-provenance.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

Provenance records binary, source, harness and raw-report digests. Timing and concurrent checks use the same binary in separate cohorts. Raw commands, reports and audit records remain in local `.data/`.

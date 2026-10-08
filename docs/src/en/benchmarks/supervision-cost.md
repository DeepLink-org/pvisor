# How much time do review, application and disposal take?

## Main conclusions {#conclusions}

**Creating a fresh task workspace, changing 20 of 10,000 files and retaining ten takes 141 ms at complete machine-workflow P50 with pVisor stage, 248 ms with Git worktree and 343 ms with a btrfs reflink copy. Stage suits sparse changes in large workspaces; Git costs less in small workspaces. A separate experiment on a prepared twenty-file view measures review/application/disposal at P50 of 27.71 ms for stage and 4.69 ms for Git.**

Applying prepared changes alone takes pVisor apply P50 of **17 ms, 0.93 s and 14.26 s** for 10, 1,000 and 10,000 small text files, all higher than Git patches. At 100,000 files, two clusters have medians of **109.73 s and 196.87 s**. The complete workflow applies only ten files; its 141 ms cannot estimate the cost of applying all 10,000 files.

| User scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable task view | Stage reduces whole-tree creation, scanning and disposal costs |
| Small workspace or a prepared, reused Git worktree | Git is faster for small workspaces and review of a prepared twenty-file view; long-term reuse is unmeasured |
| Estimate human supervision cost | Machine timings exclude human reading and decisions |

## Motivation {#motivation}

An Agent task needs a private workspace, diff review, selective application and disposal as well as tool execution. Individual filesystem timings omit these costs. If you frequently create task workspaces, compare the entire workflow that produces the same result.

Also estimate review cost with an already prepared view, and waiting and recovery costs as the number of applied files grows. Three independent experiments answer these questions without pooling samples.

## Experiment design {#interpretation}

### Complete task workflow: B-WORKFLOW

The controls are pVisor rootless stage, a detached Git worktree and a btrfs reflink copy. Inputs are identical prepared, committed and packed Git repositories containing either 100 or 10,000 files of 4 KiB each. Each task changes twenty files, produces their complete content diffs, applies the first ten and discards the rest. Git and reflink workflows extract a patch for the selected paths with `git diff`, run `git apply --check`, then apply it.

Timing covers task-view creation, executing edits, content review, selective application and disposal, summed across machine steps. Stage's view creation is included in run. Preparing input repositories, resetting every control to an identical starting point and harness correctness checks are excluded. Prepared, long-lived worktree reuse is unmeasured.

Linux/x86_64, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64, btrfs; the entire execution tree is pinned to host CPUs 0,1. Each backend, size and application condition has thirty independent samples and three warmups, randomized and interleaved with a fixed seed and warm caches. Git automatic maintenance is disabled; reflink is required and cannot fall back to a regular copy. Downloaded provenance identifies frozen artifacts and source digests.

Normal application and a host conflict are separate conditions. Every sample must pass complete file inventory and content checks: execution leaves the original unchanged; normal application changes only the ten selected files. The conflict case changes one selected host file before application and requires refusing the entire application while preserving all host content. Stage also requires Run Bundle evidence of staging and rootless isolation. Failures are retained and counted, never timed as zero; no samples are excluded for their speed. Two sizes × two conditions × three backends produce 360 measured samples, with zero failures.

This fixed file-editing task excludes inference, compilation and human reading. Git/reflink execute native processes with different isolation. Conflict checks do not cover races between Git's check and write, or compare crash recovery and durability guarantees. These results do not rank containers, VMs or security.

### Prepared-view review: B-SUPERVISION

A separate experiment measures only review of a prepared view: twenty small text files already contain identical edits; review every original/edited content, apply the first ten and discard the other ten. Stage timing includes `status --review --diff`, selective `apply` and `drop`. Git timing includes complete diff review, selected patch extraction, `git apply --check`, application and worktree removal. View creation, task execution, fixtures and validation are excluded. Git 2.55.0 is identified by executable digest before sampling; the frozen stage artifact has separate provenance. Each group has thirty samples and three warmups, CPUs 0 and 1, warm caches and no benchmark-specific host memory cap. All sixty formal samples pass content, selection and execution-boundary checks, with final workspaces retained for independent audit and no speed-based exclusions. Prepared-view review is summarized separately and is not added to the complete-workflow table.

### Application scale and recovery: B-APPLY

The host is an AMD Ryzen 7 9700X running Linux 7.2.8-200.fc44 x86_64. Measured processes are pinned to CPUs 0 and 1, with no benchmark-specific host memory cap. Caches are warm and the page cache is not actively cleared. The workload updates small text files in one directory; staged changes and Git patches are generated before timing. Workspace preparation and subsequent verification are outside timing.

At 10, 1,000, 10,000 and 100,000 files, measure pVisor apply, Git apply, native copying, drop and rejection of a conflict introduced before application. Each condition has three warmups and 30 samples, totaling 600 formal timings; a fixed seed randomizes size and operation order within each round. Successful samples must pass complete target-content checks and applicable application-ledger checks. Failures are retained separately, and all valid slow samples are retained without exclusions based on duration.

Independent correctness checks include three actual SIGKILL hits at each of three durable states in a 10,000-file application, and three host-edit injections during application using the same binary. For the latter, pause after one file has changed while the file to be edited still has its original content, write and synchronize the external edit, then resume. These checks cover finite injection windows, not every concurrent schedule.

## Data and analysis {#results}

### Complete task cost {#baseline-meaning}

Same host and batch, 2026-10-06; milliseconds, N=30 per cell. P95 is descriptive only. No cell meets the predefined separated-cluster rule.

| Workspace files | pVisor stage P50 / P95 | Git worktree P50 / P95 | btrfs reflink P50 / P95 |
|---|---:|---:|---:|
| 100 | 109.02 / 110.56 | 22.02 / 23.14 | 23.66 / 25.28 |
| 10,000 | 140.82 / 144.38 | 248.10 / 253.89 | 343.00 / 349.73 |

For 10,000 files, the stage-minus-Git median difference is **-107.27 ms, 95% CI [-109.02, -105.60]**; against reflink it is **-202.18 ms, 95% CI [-204.49, -199.81]**. Intervals use 5,000 bootstrap resamples paired by randomized sampling round and support stage being faster in this condition. For 100 files, stage minus Git is **+86.99 ms, 95% CI [+77.28, +87.57]** and stage minus reflink is **+85.36 ms, 95% CI [+75.45, +85.86]**; native workflows are faster in that small-workspace condition.

### Review cost with a prepared view {#prepared-review}

Separate matched controls measured on 2026-10-06: twenty small text files, ten applied and ten discarded, N=30 per cell, zero failures. Milliseconds; P95 is descriptive. No cell triggers the split rule.

| Step | pVisor stage P50 / P95 | Git worktree P50 / P95 |
|---|---:|---:|
| Review complete content | 6.51 / 7.42 | 1.28 / 1.47 |
| Select, check and apply ten files | 17.42 / 19.25 | 2.53 / 3.52 |
| Dispose of remaining results | 3.60 / 3.87 | 0.86 / 1.07 |
| Per-sample workflow total | 27.71 / 29.48 | 4.69 / 5.86 |

The stage-minus-Git total median difference is **+23.02 ms, 95% CI [22.49, 23.49]**, using 5,000 bootstrap resamples paired by sampling round. Git is faster in this small prepared-view condition. Step medians do not sum to the workflow median; human reading is unmeasured. Stage provides execution boundaries, staging and review interfaces; this small-edit review workflow has no measured speed advantage.

### Where the cost lies

Normal application with 10,000 files; P50 milliseconds, N=30 per cell. Step medians do not sum to the complete workflow median.

| Step | pVisor stage | Git worktree | btrfs reflink |
|---|---:|---:|---:|
| Create task view | Included in run | 83.88 | 106.82 |
| Execute twenty edits | 84.19, including view creation | 13.31 | 13.43 |
| Review complete content diffs | 6.95 | 73.45 | 151.10 |
| Select, check and apply ten files | 46.22 | 4.14 | 4.00 |
| Dispose of remaining task view | 3.49 | 73.31 | 67.54 |

Stage still adds tool-execution and application costs, but creating its private view, reviewing the changeset and discarding it do not require processing all 10,000 files. Git/reflink whole-tree work outweighs stage's additional costs in this sparse-edit workflow. This supports choosing stage for frequently created large task workspaces; it does not establish faster individual reads, writes or compilation.

### Cost of preserving a host conflict {#acceptance}

One selected host file is changed before application. The complete workflow includes creation, execution, review, refusal and disposal. P50 milliseconds, N=30 per cell.

| Workspace files | pVisor stage | Git worktree | btrfs reflink | Refused with all host content preserved |
|---|---:|---:|---:|---|
| 100 | 95.64 | 21.66 | 23.37 | 30/30 for each backend |
| 10,000 | 98.37 | 248.00 | 342.22 | 30/30 for each backend |

No participant study was conducted. Batch review and machine timing cannot be converted into human time savings.

### Application and disposal by scale {#apply-cost}

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

### What the Git patch baseline means {#apply-baseline}

Median differences use 5,000 bootstrap resamples of paired rounds with a fixed seed. Units: ms; 30 pairs per row.

| Files | pVisor apply − Git apply | 95% confidence interval for the difference |
|---:|---:|---:|
| 10 | +16.13 | +15.79 to +18.63 |
| 1,000 | +911.41 | +895.05 to +1,017.69 |
| 10,000 | +14,104.40 | +11,179.77 to +19,418.50 |

All three scales show higher pVisor latency. The separated distribution at 100,000 files is not summarized by a single median difference. Git patches also check patch context, but do not provide the same preimage checks, durable application ledger and interruption-recovery workflow. This comparison estimates waiting time for the same text updates; it does not claim identical transaction semantics.

### Host edits during apply {#apply-conflicts}

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

### Scope {#apply-scope}

The data covers warm caches and small text files in one directory. It does not cover large binaries, all file types, symlinks and metadata combinations, or macOS. Full content validation ran during measurement; retained commands and ledgers were independently audited, but cleaned target directories cannot be reread byte for byte. Finite conflict injections do not prove detection of every concurrent edit.

### Application-scale and recovery data {#apply-downloads}

[Scale timings CSV](apply.csv) · [Paired Git comparison CSV](apply-comparisons.csv) · [Recovery checks CSV](apply-recovery.csv) · [Concurrent conflicts CSV](apply-concurrent-conflicts.csv) · [Provenance CSV](apply-provenance.csv)

Provenance records binary, source, harness and raw-report digests. Timing and concurrent checks use the same binary in separate cohorts. Raw commands, reports and audit records remain in local `.data/`.

### Downloads and reproduction {#run}

[Derived table CSV](supervision-cost.csv) · [Step statistics](workflow-summary.csv) · [Differences and confidence intervals](workflow-comparisons.csv) · [Sources and artifacts](workflow-provenance.csv) · [Prepared-view steps](supervision-summary.csv) · [Prepared-view paired differences](supervision-comparisons.csv) · [Prepared-view provenance](supervision-provenance.csv) · [Evidence source summary](evidence-sources.csv)

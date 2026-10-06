# Which workflow costs less: a private task workspace, review and selective application?

## Main conclusions {#conclusions}

**Creating a fresh task workspace, changing 20 of 10,000 files and retaining ten takes 141 ms at complete machine-workflow P50 with pVisor stage, 248 ms with Git worktree and 343 ms with a btrfs reflink copy. Stage suits sparse changes in large workspaces; Git costs less in a 100-file workspace.**

| User scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable task view | Stage reduces whole-tree creation, scanning and disposal costs |
| Small workspace or a prepared, reused Git worktree | Git is faster for small workspaces; workspace reuse is unmeasured |
| Estimate human supervision cost | Machine timings exclude human reading and decisions |

## Motivation {#motivation}

An Agent task needs a private workspace, diff review, selective application and disposal as well as tool execution. Individual filesystem timings omit these costs. If you frequently create task workspaces, compare the entire workflow that produces the same result.

## Experiment design {#interpretation}

The controls are pVisor rootless stage, a detached Git worktree and a btrfs reflink copy. Inputs are identical prepared, committed and packed Git repositories containing either 100 or 10,000 files of 4 KiB each. Each task changes twenty files, produces their complete content diffs, applies the first ten and discards the rest. Git and reflink workflows extract a patch for the selected paths with `git diff`, run `git apply --check`, then apply it.

Timing covers task-view creation, executing edits, content review, selective application and disposal, summed across machine steps. Stage's view creation is included in run. Preparing input repositories, resetting every control to an identical starting point and harness correctness checks are excluded. Prepared, long-lived worktree reuse is unmeasured.

Linux/x86_64, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64, btrfs; the entire execution tree is pinned to host CPUs 0,1. Each backend, size and application condition has thirty independent samples and three warmups, randomized and interleaved with a fixed seed and warm caches. Git automatic maintenance is disabled; reflink is required and cannot fall back to a regular copy. Downloaded provenance identifies frozen artifacts and source digests.

Normal application and a host conflict are separate conditions. Every sample must pass complete file inventory and content checks: execution leaves the original unchanged; normal application changes only the ten selected files. The conflict case changes one selected host file before application and requires refusing the entire application while preserving all host content. Stage also requires Run Bundle evidence of staging and rootless isolation. Failures are retained and counted, never timed as zero; no samples are excluded for their speed. Two sizes × two conditions × three backends produce 360 measured samples, with zero failures.

This fixed file-editing task excludes inference, compilation and human reading. Git/reflink execute native processes with different isolation. Conflict checks do not cover races between Git's check and write, or compare crash recovery and durability guarantees. These results do not rank containers, VMs or security.

## Data and analysis {#results}

### Complete task cost {#baseline-meaning}

Same host and batch, 2026-10-06; milliseconds, N=30 per cell. P95 is descriptive only. No cell meets the predefined separated-cluster rule.

| Workspace files | pVisor stage P50 / P95 | Git worktree P50 / P95 | btrfs reflink P50 / P95 |
|---|---:|---:|---:|
| 100 | 109.02 / 110.56 | 22.02 / 23.14 | 23.66 / 25.28 |
| 10,000 | 140.82 / 144.38 | 248.10 / 253.89 | 343.00 / 349.73 |

For 10,000 files, the stage-minus-Git median difference is **-107.27 ms, 95% CI [-109.02, -105.60]**; against reflink it is **-202.18 ms, 95% CI [-204.49, -199.81]**. Intervals use 5,000 bootstrap resamples paired by randomized sampling round and support stage being faster in this condition. For 100 files, stage minus Git is **+86.99 ms, 95% CI [+77.28, +87.57]** and stage minus reflink is **+85.36 ms, 95% CI [+75.45, +85.86]**; native workflows are faster in that small-workspace condition.

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

### Downloads and reproduction {#run}

[Derived table CSV](supervision-cost.csv) · [Step statistics](workflow-summary.csv) · [Differences and confidence intervals](workflow-comparisons.csv) · [Sources and artifacts](workflow-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

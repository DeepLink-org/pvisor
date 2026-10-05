# What is the machine cost of batch review and selective application?

## Main conclusions {#conclusions}

**Reviewing 20 files, applying ten and dropping the other ten takes about **25 ms** at machine-side P50. That cost suits interactive use. Human reading and decision time is unmeasured, so no percentage reduction in supervision time is established. Git/diff workflows can also batch review.**

| Need | Selection implication |
|---|---|
| Batch review and path selection | Machine steps fit an interactive flow |
| Existing Git/diff review workflow | No equivalent timing ranking is available |
| Estimate human supervision cost | Requires a separate participant study |

## Motivation {#motivation}

Agent speed is only part of the experience: approvals, diff reading and conflict resolution also matter. Machine overhead and human supervision need separate evidence.

## Experiment design {#interpretation}

30 independent stages, no warmups. Each run checks all 20 review items, applies 10 paths and verifies target contents, then drops the remaining 10 and verifies unchanged lower files. Wall time is the three machine steps summed; it excludes stage generation, user waiting and reading.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

| Step | N | P50 / P95 ms |
|---|---|---|
| review_ms | 30 | 3.21 / 7.95 |
| apply_ms | 30 | 17.62 / 43.66 |
| drop_ms | 30 | 4.07 / 12.83 |
| wall_ms | 30 | 24.94 / 64.74 |

### Decisions and interpretation

Per-tool approval count depends on tool requests. Stage review can decide multiple changes together, while still requiring diff reading, conflict handling and path selection. Docker/Git can also batch review. There are no human participants here; batching files does not establish a 90% reduction in human time.

These timings support a low machine cost for an automated review flow. They do not establish user satisfaction, decision accuracy or an optimal approval policy.
### Baseline and interaction budget {#baseline-meaning}

The familiar reference workflow is inspecting changes with Git/diff and selecting files to keep. An equivalent Git review workflow was not timed in this batch. The measured roughly 25 ms covers machine work for listing, filtering, and committing, which fits within an interaction. It does not mean a person can review changes in 25 ms or establish saved human time. Human review performance still requires a separate experiment.

### Scope {#acceptance}

Human reading/decision time and real-team review success are unmeasured. Machine timings do not establish a percentage labor saving.


# Roadmap

See [Trust ladder](../why/trust-ladder.md) for the levels and scale axes. The table below lists ongoing L1 work (individual local Jobs) and its acceptance criteria.

| Work | Acceptance criteria |
| --- | --- |
| Local lifecycle/staging reliability | Regressions for completion, cancellation, timeout, fork and batch apply on macOS/Linux; inspect records and remaining changes after failure |
| Boundaries agree with evidence | Verify file/network controls per executor; Bundles distinguish plans, installation receipts, unobserved counts and zeros; labels cannot hide missing controls |
| Gateway capture reliability | Regressions for bounded queues, commit failures, shutdown and recovery; distinguish enqueue from durable commit |
| Replay compatibility | Pin supported versions per adapter; verify complete tool batches and the first continuation-request boundary; reports state samples and limitations |
| Documentation/distribution consistency | Entry examples run; each default behavior has one authoritative definition |

Before adding a public feature, provide implementation, validation scenarios, limitations and release notes. Define compatibility and acceptance before changing data contracts, boundaries or public commands.

## L2 and L3 milestones

!!! note "Under construction"
    The entry criteria and gap list for L2/L3 are not final; this section collects the requirements and acceptance criteria written down so far and is not a schedule or a maturity claim. For the level definitions see the [trust ladder](../why/trust-ladder.md); for the capacity rationale see [concurrency density (planned)](../benchmarks/density.md).

### L2: multiple local Jobs / one pipeline

Entry criteria (TBD): a measured per-machine concurrency limit and resource model, used as the capacity planning basis; see [concurrency density (planned)](../benchmarks/density.md).

Acceptance criteria:

- Batch review is reproducible: aggregate by workspace and apply in batches; see [run many agents on one host and review them in batches (planned)](../guides/parallel-agents.md).
- CI integration provides a copyable workflow example, states the meaning of `apply` (who reviews, when it merges), and has regressions for failure and timeout paths; see [run agents in CI (planned)](../guides/ci.md).

### L3: clustered execution, centralized evidence

Entry criteria (TBD): the boundary with schedulers is defined—pVisor provides execution semantics and evidence, while cross-node orchestration goes to Kubernetes and Ray; see [cluster execution (planned)](../design/research/cluster-execution.md).

Acceptance criteria:

- Give a gap list and staging plan for cross-node scheduling and for auditing after evidence is centralized; see [cluster execution (planned)](../design/research/cluster-execution.md).
- The capacity rationale is shared with L2: [concurrency density (planned)](../benchmarks/density.md).

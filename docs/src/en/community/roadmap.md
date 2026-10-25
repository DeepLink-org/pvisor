# Roadmap

The current focus is single-machine execution reliability, shared-resource cost and interface consistency. Native Jobs, per-container VMs and daemon SDK profiles have separate entry points; the work below completes their contracts and validation without defining supervision levels by instance count.

| Work | Acceptance criteria |
| --- | --- |
| Local lifecycle/staging reliability | Regressions for completion, cancellation, timeout, fork and batch apply on macOS/Linux; inspect records and remaining changes after failure |
| Boundaries agree with evidence | Verify file/network controls per executor; Bundles distinguish plans, installation receipts, unobserved counts and zeros; labels cannot hide missing controls |
| Gateway capture reliability | Regressions for bounded queues, commit failures, shutdown and recovery; distinguish enqueue from durable commit |
| Replay compatibility | Pin supported versions per adapter; verify complete tool batches and the first continuation-request boundary; reports state samples and limitations |
| Documentation/distribution consistency | Entry examples run; each default behavior has one authoritative definition |

Before adding a public feature, provide implementation, validation scenarios, limitations and release notes. Define compatibility and acceptance before changing data contracts, boundaries or public commands.

## Installation and integration {#installation-and-integration}

Planned: make installation, first execution and staged review work end to end before expanding distribution channels and language SDKs. The [distribution proposal](releasing.md#distribution-plan) defines channel roles and shared artifacts; [installation](../start/installation.md) owns currently available commands.

Next are standalone binary archives, a versioned installer and a Homebrew entry point, alongside installation diagnostics, upgrades, rollback and removal. Settle CLI JSON output, events, errors and compatibility first; actual platform integration needs then drive TypeScript/Python SDKs. Evaluate npm distribution independently of an SDK; neither is a prerequisite for the CLI release.

Linux host selective egress still uses a cooperative proxy, so the `--safe` label cannot establish a non-bypassable boundary. The proposed work combines a controlled proxy exit with host network isolation and validates cleared proxy variables, direct sockets and access to helper processes. See [network boundaries](../guides/policies/network.md#网络边界) for existing enforced-egress options and platform differences.

## Small-file delivery for lazy images {#lazy-image-small-files}

Planned: automatically pack small files during image build/publication to reduce remote requests and cold-read waits for S3-backed lazy images. No implementation or performance acceptance results are available yet. See [shared image cache V1](../design/shared-image-cache-storage.md) for the current format and limitations, and [Lazy Image V2](../design/lazy-image-v2.md) for the detailed proposal on directory packing, deterministic bucketing and two-level indexes.

Design direction:

- Preserve independent file paths, permissions, hardlinks, symlinks and xattrs. Group physical contents into immutable packs by access locality, such as package or directory, with indexes locating offsets and lengths.
- Separate object size, read/compression block size and cache granularity. Support range reads and independently decompressible blocks without requiring full-pack download or decompression.
- Combine local metadata caching, adjacent-read coalescing, bounded prefetch and concurrent-miss coalescing. Packing objects while retaining one request per file does not complete the optimization.
- Preserve cross-image content sharing rather than repacking common dependencies for every image; keep on-demand chunk reads for large files. Evaluate reusable Nydus/EROFS capabilities before extending the custom format.

Acceptance criteria:

- Compare independent small-file objects, per-file range reads within packs, and packs with read coalescing/caching. Cover cold metadata, cold content, warm caches and concurrent tasks separately.
- Use Python imports, Node.js dependency loading, directory/attribute scans and real builds/tests. Record first useful tool call, complete-task time and P95/P99, request counts, downloaded bytes, read amplification, cache occupancy, packing time and total cost. Include first-publication costs and evaluate low-reuse environments separately. Register experiments and follow benchmark publication rules without assuming a speedup percentage.
- Verify file semantics, copy-up, change delivery, content integrity and remote-failure behavior. Before changing the format, define compatibility/migration for existing image handles and checkpoint dependencies, plus pack retention and safe reclamation boundaries.

## Single-node daemon {#daemon}

The [daemon](../guides/daemon/index.md) provides image preparation, a default native guest engine and real-VM validation of the OpenSandbox 1.1.0/E2B Python SDK 2.59.0 core flows. Next work expands protocol regressions, lifecycle failure recovery and fixed-budget measurements. Stage/apply, checkpoint/fork APIs and automatic node-resource acquisition remain unwired; full SDK conformance, production density and hostile multi-tenant assurance require separate acceptance.

## L2 and L3 milestones

!!! note "Under construction"
    The entry criteria and gap list for L2/L3 are not final, and no schedule is set. The requirements and acceptance criteria below describe the current work. For the level definitions see the [supervision levels and execution scale](#trust-ladder); for the capacity rationale see [concurrency density](../benchmarks/density.md).

### L2: policy and evidence gates

Entry criteria (TBD): define exemption rules, evidence requirements and failure fallback over existing local concurrency; use measured concurrency limits and resource models for capacity planning; see [concurrency density](../benchmarks/density.md).

Acceptance criteria:

- Batch review is reproducible: aggregate by workspace and apply in batches; see [run many agents on one host and review them in batches](../guides/parallel-agents.md).
- CI integration provides a copyable workflow example, states the meaning of `apply` (who reviews, when it merges), and has regressions for failure and timeout paths; see [run agents in CI](../guides/ci.md).

### L3: clustered execution, centralized evidence

Entry criteria (TBD): the boundary with schedulers is defined—pVisor provides execution semantics and evidence, while cross-node orchestration goes to Kubernetes and Ray; see [cluster execution](../design/research/index.md).

Acceptance criteria:

- Give a gap list and staging plan for integration with external cross-node schedulers and for auditing after evidence is centralized. pVisor does not provide a cluster-wide control plane; the single-node daemon has no global DAG or distributed lease.
- The capacity rationale is shared with L2: [concurrency density](../benchmarks/density.md).

## Supervision levels and execution scale {#trust-ladder}

The L0–L3 labels remain a reference for supervision: L0 approves individual commands, L1 reviews individual results, L2 spot-checks using policy and evidence, and L3 focuses on audits and exceptions. Execution scale can be one task, local concurrency, or multiple machines scheduled by an external system; scale and human involvement are independent axes. Increasing concurrency does not establish L2/L3 acceptance rules. The milestones above still require their respective entry conditions and acceptance criteria.

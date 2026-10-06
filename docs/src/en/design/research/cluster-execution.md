# Cluster-scale execution and centralized evidence

Scale execution across hosts through **external orchestration**, while keeping each execution's boundary and evidence explicit. Kubernetes, Ray or a training framework may own host selection and business workflows; pVisor does not replace them with a distributed product control plane.

## Local execution building blocks {#execution-flow}

Ordinary `pvisor run` owns a Job/Run/Attempt through native executors, optional staging and Gateway, and produces execution records. The [single-node daemon](../daemon/index.md) instead owns local native VM sandbox admission, lifecycle, TTL and service endpoints. Its partial OpenSandbox profile does not automatically supply native Job semantics, VM restore, stage/apply, offload or centralized Run Bundles.

Native node resources can retain immutable environment/backing ownership on one host, independently of the daemon. They do not share physical RAM across machines and are not integrated with the daemon backend. See [shared working sets](../daemon/shared-working-set.md).

The earlier Controller/Worker implementation explored distributed execution and evidence delivery. Its archived measurements retain that historical scope; they do not describe the new daemon or validate a current distributed product.

## Product and external integration boundaries {#boundary}

| Layer | Current responsibility | Integration/validation remaining |
| --- | --- | --- |
| Native pVisor execution | Job lifecycle, executor controls, staging, optional Gateway and records | Orchestrator adapters and representative workload acceptance |
| Sandbox daemon | NativeRuntime VM admission, durable supervisor ownership, lifecycle and real-service proxy | Supplied/validated bootstrap and SDK profile; no stage/checkpoint API or density evidence |
| Native node resources | Same-host immutable ownership, pins and bounded warmth | Daemon wiring, full transient accounting and workload measurements |
| External orchestration | Host choice, queues, dependencies, retries and tenancy | Explicit execution/evidence handoff and business-effect reconciliation |
| Evidence/checkpoint repositories | Native integrity, publication and retention contracts | Central collection, authorization, replication and cross-host compatibility |
| Review/release services | Consume explicitly retained outputs | Baseline checks, selective acceptance, business reconciliation and publication |

A local record path or uploaded JSON is not a complete transfer of files. A downloadable checkpoint does not prove arbitrary-host restore compatibility. Define pinned inputs, runtime profiles, model versions and storage authorization before interpreting recovery as equivalent execution.

## Experiments to design {#validation}

Compare external orchestration using pVisor execution boundaries against pinned Kubernetes/Ray or training-framework baselines. Fix workloads, resources, models and output checks; measure useful completion throughput, first-result/restore tails, whole-group memory and centralized-review cost. Extend [concurrent-density methodology](../../benchmarks/density.md), without extrapolating daemon gains from historical empty tasks or native sharing mechanisms.

Inject host loss, daemon/orchestrator downtime, partitions, full disks, interrupted collection and credential revocation. Track orchestrator work identity, local sandbox or Job/Run/Attempt identity, native observations and artifacts separately. A timeout or control identity does not prove external effects stopped; retry policy must reconcile unknown effects.

No cross-host recovery, exactly-once external effects or daemon density advantage is established by this research direction. Current daemon [operations](../daemon/operations.md) define only local behavior.

## Research questions {#research}

- What minimal handoff preserves requested policy, installed controls, observed outcomes and complete evidence between execution and orchestration?
- Which retained inputs and runtime compatibility checks make a restore portable, and which effects require business reconciliation rather than replay?
- How much supervision does batch review save at fixed correctness and audit coverage, including collection and storage costs?
- Does immutable sharing or bounded laziness improve first useful result and full completion under equal total resources?

See [parallel agents](../../guides/parallel-agents.md) for local workflows and [RL execution substrate](rl-execution-substrate.md) for training integration. Research plans do not create a product scheduler or substitute for production acceptance.

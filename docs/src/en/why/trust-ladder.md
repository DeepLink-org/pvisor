# Trust ladder and scale

Two axes describe pVisor's direction: **how humans intervene** determines how much can be delegated; **what carries execution** determines how much can run afterward.

| Level | Human intervention | Execution scale | Status |
| --- | --- | --- | --- |
| L0 | Approve each command | One interactive session | Built into agents |
| L1 | Run unattended, review every change afterward | Local, individual Jobs | **pVisor today** |
| L2 | Policy and evidence determine exemptions; humans spot-check | Multiple local Jobs, one pipeline | Next |
| L3 | Audit afterward; humans handle exceptions | Cluster scheduling and centralized evidence | Direction |

Each level uses the same bounded, recoverable, checkable properties. Their reliability and machine usability distinguish the levels.

## L1: unattended execution, review afterward (today)

**Available:** staging and selective apply, conflict refusal, default sensitive-path protection, host/container/VM executors, evidence separating plans from installed controls, optional model capture, logical checkpoints and forks.

**Human work:** review each run and choose paths to apply.

## L2: policy and evidence determine exemptions (next)

**Entry criteria:**

- Evidence reliable enough to assess allowed changes automatically, for example changes limited to `src/`, no denied accesses, and observed network destinations within policy.
- Multiple local Jobs with individual isolation and evidence.
- Batch review with human intervention for exceptions.

**Gaps:** automated exemption rules, [concurrency density data (planned)](../benchmarks/density.md), batch review UI, and reduced review noise.

## L3: audit afterward, execute across a cluster (direction)

**Entry criteria:** centralized evidence storage/query, coordinated policy distribution, and placement across nodes.

**Scheduler boundary:** pVisor defines staging, evidence, fork, and replay semantics. Kubernetes, Ray, or Slurm allocate nodes. The direction is a per-node execution layer and evidence aggregation rather than another scheduler.

| Foundation | Missing piece |
| --- | --- |
| Host/container/VM abstraction | Remote executor or node agent |
| Placement fields in Operation | Cross-node decisions and scheduler integration |
| Host-independent Run Bundles and evidence | Central storage, queries, and audit |
| Policy admission | Distribution and tenant isolation |
| Capture, replay, and fork | Multi-tenant Gateway; cluster rollout fork/replay |

See [cluster execution (planned)](../design/research/cluster-execution.md).

## Measuring progress

**Human supervision cost per unit of agent work** should fall from L1 to L3 without reducing task success. See [supervision cost (planned)](../benchmarks/supervision-cost.md).

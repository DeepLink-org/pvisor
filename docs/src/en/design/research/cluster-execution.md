# Cluster execution and centralized evidence

When integrating pVisor into a cluster, let the scheduler choose a machine and let pVisor execute the task on that worker and retain a reviewable result. The current CLI provides a local execution entry. Cross-node scheduling, unified identity, and centralized storage require a separate integration layer.

## A task's path through a worker {#worker-flow}

1. The scheduler assigns a task ID, versioned workspace input, command, resource budget, and policy.
2. The worker prepares an independent checkout and an external Stage, then checks executor capabilities.
3. The worker starts pVisor and saves the exit code, Run ID, and native agent trajectory.
4. After execution, it collects the Bundle, Stage, logs, and optional Gateway Journal.
5. The control layer presents results by task ID for acceptance, retry, or discard.

Index failures too. Setup failures may have no Bundle; retain scheduler records, exit status, and stderr. Retries receive new attempt IDs and preserve old attempts rather than overwriting failure evidence.

## Boundary between existing interfaces and integration work {#boundary}

| Layer | Responsibility |
| --- | --- |
| pVisor worker | One execution, staged files, installed-control observations, and task records |
| Kubernetes, Ray, or a custom scheduler | Queues, node selection, resource reservation, and retry policy |
| Evidence storage | Artifact upload, indexing, retention, and access permissions |
| Review and release service | Baseline validation, change selection, merge, and deployment |

Treat local Bundle paths as worker-side references. Uploading JSON does not upload every referenced file, and downloading a Stage does not make it applicable to a different checkout. Central services need artifact packaging and an explicit mapping between paths and baselines.

## Validate an integration first {#validation}

Start on two workers with a successful small task, a timeout, and a policy rejection. Check that every attempt can be traced to its input version, outcome, and artifacts, and that interrupted uploads or duplicate submissions cannot overwrite another result.

Then test node loss, full disks, revoked credentials, and cancellation. Configure cache permissions by worker and user boundary. Record requested limits and installed controls in evidence, keeping scheduler resource reservations separate.

## Research questions and release conditions {#research}

Cross-node identity and permissions, artifact portability, idempotent retries, centralized cancellation, garbage collection, and capability admission on heterogeneous machines still need validation. Measure throughput, tail latency, and recovery under fixed workloads; single-machine empty tasks cannot establish cluster capacity.

Start from [Parallel agents](../../guides/parallel-agents.md) and consult [Concurrent density](../../benchmarks/density.md) for metrics. A formal cluster interface needs implementation, a version contract, and fault experiments published together.

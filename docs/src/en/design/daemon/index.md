# Single-node daemon architecture

Manage local sandboxes through `pvisor-daemon`: one process accepts resource-bounded creation requests, persists ownership and intentions, controls the native runtime, expires sandboxes and routes access to their prepared services. External orchestration chooses hosts and coordinates business work; it is not a pVisor product control plane.

## Responsibilities {#architecture}

```text
Caller / external orchestration
  → OpenSandbox lifecycle HTTP API
  → daemon: local admission + durable registry + per-sandbox lifecycle locks
  → Runtime adapter → external rootless Podman
  → prepared image: workload + real execd + capability-free egress service
Caller → daemon endpoint proxy → prepared service
```

| Owner | Responsibility | Outside its scope |
| --- | --- | --- |
| Caller | Workload, prepared image, input versions, retries and business effects | Assuming a timeout proves no execution occurred |
| Daemon | Local admission, ownership, intentions, runtime reconciliation, TTL, authenticated endpoints | Cross-host scheduling or training transactions |
| External Podman | Container execution, cgroup controls, namespaces, native observations | pVisor Job staging, VM checkpoints or model evidence |
| Prepared image | Supervise argv, initialize and authenticate execd/egress | Replacing lifecycle API authorization |
| Native pVisor executor and node resources | Separate Job/VM semantics and immutable backing ownership | Automatic integration with this daemon |

The daemon crate uses a `Runtime` trait rather than depending on `pvisor`, which already depends on this package. Its only current backend is external rootless Podman. There is no host fallback or native VM backend.

## Create and access {#task-flow}

1. Validate the image, argv, environment, metadata, hard CPU/memory limits and optional TTL. Reject unsupported controls before admission.
2. Under registry serialization, check local capacity and durably insert a random `sb-*` identity, reservation and `Pending` record.
3. With that sandbox's lifecycle lock held, create/start the labeled container and check installed resource settings and actual service readiness.
4. Persist `Running` only after native creation succeeds. Verified failed-create cleanup releases the record; uncertain cleanup retains its identity and reservation.
5. Resolve supported service endpoints through the daemon. Inspect reconciles native state; deletion persists intent and confirms native absence before releasing capacity.

This is sandbox management, not a task/result protocol. Sandbox IDs do not imply Job/Run/Attempt identities or a Run Bundle.

## Design map {#documents}

| Question | Design |
| --- | --- |
| What can this machine admit? | [Local admission](admission.md) |
| When is a control operation complete? | [Lifecycle](lifecycle.md) |
| What survives a restart? | [State and recovery](state-and-recovery.md) |
| What is stored and reclaimed? | [Storage](storage.md) |
| How do you deploy and diagnose it? | [Operations](operations.md) |
| Where do immutable sharing and lazy reads belong? | [Shared working sets](shared-working-set.md) |
| Which services should remain separate? | [Responsibility convergence](responsibility-convergence.md) |

## Compatibility and evidence {#invariants}

The partial API profile is pinned to **OpenSandbox 1.1.0**, `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`. It is not full API or unmodified SDK end-to-end conformance. The prepared execd/egress image contract has no validated end-to-end recipe yet; see [operations](operations.md#image-contract).

Native VM, stage/apply, checkpoint/fork, offload, Gateway inference-wait coordination and node-resource acquisition are not wired into this backend. Earlier Cluster measurements belong to the retired distributed implementation, not daemon performance. No daemon density advantage or node-wide physical-memory benefit has been validated.

Implementation ownership: `crates/pvisor-daemon/src/daemon/{models,store,mod,api}.rs`, `runtime.rs` and `main.rs`. Legacy feature-gated modules are transitional code, not this architecture.

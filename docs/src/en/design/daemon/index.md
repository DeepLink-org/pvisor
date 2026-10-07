# Single-node daemon architecture

Manage local sandboxes through `pvisor-daemon`: one process accepts resource-bounded creation requests, persists ownership and intentions, controls the native runtime, expires sandboxes and routes access to their prepared services. External orchestration chooses hosts and coordinates business work; it is not a pVisor product control plane.

## Responsibilities {#architecture}

```text
Caller / external orchestration
  → OpenSandbox lifecycle HTTP API
  → daemon: local admission + durable registry + per-sandbox lifecycle locks
  → NativeRuntime → detached supervisor embedding pvisor → VM only
  → prepared image: workload + real execd + egress service with guest CID 3 vsock bridges
Caller → daemon endpoint proxy → prepared service
```

| Owner | Responsibility | Outside its scope |
| --- | --- | --- |
| Caller | Workload, prepared image, input versions, retries and business effects | Assuming a timeout proves no execution occurred |
| Daemon | Local admission, ownership, intentions, runtime reconciliation, TTL, authenticated endpoints | Cross-host scheduling or training transactions |
| Native supervisor | Embedded pVisor/VM, RunHandle, acknowledged vCPU controls, cgroup identity, vsock bridges | Stage/apply/checkpoint API or automatic node sharing |
| Prepared image | Supervise argv, initialize and authenticate execd/egress | Replacing lifecycle API authorization |
| Native pVisor | VM execution embedded by supervisors; public Job workflow remains separate | Automatic exposure of stage/checkpoint APIs |
| Native node runtime | Separate immutable backing protocols | Automatic daemon acquisition/sharing |
| Daemon pool component | Optional detached pool enabled by `serve --memory-pool` | Pool-process/host-reboot recovery; node acquire/release |
| Independent `pvisor-cache` | OCI prepare/publish/serve/list/stat/read | Daemon lifecycle ownership |

The Runtime trait has one implementation: VM-only NativeRuntime. Detached supervisors embed `pvisor::PVisor` with only VmExecutor and retain RunHandles across daemon restart. No host/OCI/pull fallback exists. The executable's `serve` command constructs NativeRuntime using required `--images-dir`/`--cgroup-root`; synchronous internal VM dispatch runs before Tokio, and the hidden supervisor command is dispatched. See [operations](operations.md#deployment).

## Create and access {#task-flow}

1. Validate the image, argv, environment, metadata, hard CPU/memory limits and optional TTL. Reject unsupported controls before admission.
2. Under registry serialization, check local capacity and durably insert a random `sb-*` identity, reservation and `Pending` record.
3. Persist native preparation/identity, install identity-bound cgroup limits, place the child in that cgroup before exec, launch the detached supervisor and verify acknowledged live VM controls plus real service readiness.
4. Persist `Running` only after native creation succeeds. Verified failed-create cleanup releases the record; uncertain cleanup retains its identity and reservation.
5. Resolve supported service endpoints through the daemon. Inspect reconciles native state; deletion persists intent and confirms native absence before releasing capacity.

This is sandbox management, not a task/result protocol. Private runtime records bind generation to native Run/Attempt IDs, but `sb-*` is not a public Job ID and the API does not expose Job review, checkpoints or Run Bundle export.

## Concurrency and failure boundaries {#documents}

The [registry commit lock](storage.md#commit) serializes capacity checks and durable mutations. A per-sandbox lifecycle lock orders controls and proxy connection establishment; slow VM operations do not hold a global runtime mutex. This keeps unrelated sandboxes moving while preventing two creations from spending the same reservation or a connection from racing daemon-managed deletion.

Durable intentions and live observations have different authority. Persist `Stopping` before native deletion, and release capacity only after absence is confirmed. Lost IPC or an acknowledgement leaves work to reconcile; it cannot authorize a replacement VM or reusable capacity. A [daemon-only restart](state-and-recovery.md#reconcile) reconnects surviving supervisors using the same ownership state. Host reboot loses live VMs.

Conservative reservations trade utilization for explicit cleanup accounting: paused, failed and uncertain records remain charged until verified removal. Low RSS and native cache sharing do not reduce those reservations; [admission](admission.md#reservations) and physical working-set accounting remain separate.

## Compatibility and evidence {#invariants}

The partial API profile is pinned to **OpenSandbox 1.1.0**, `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`. It is not full API or unmodified SDK end-to-end conformance. The prepared execd/egress image contract has no validated end-to-end recipe yet; see [operations](operations.md#image-contract).

Native VM execution is wired in the runtime. Stage/apply, checkpoint/fork and offload APIs, Gateway inference-wait coordination and automatic node-resource acquisition are not implemented. Historical Cluster results do not validate daemon SDK conformance, performance or density; no node-wide physical-memory gain is established.

Implementation ownership: `crates/pvisor-daemon/src/daemon/{models,store,mod,api}.rs`, `runtime.rs` and `main.rs`. The retired Cluster implementation is not this architecture.

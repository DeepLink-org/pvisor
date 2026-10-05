# Native controls, inference waits and forks

Cluster separates control intent, native execution and success observations. An administrative request returns an accepted record; only an exact-identity Worker ACK demonstrates native success and triggers accounting changes.

## Control protocol {#control}

`ControlRequest` is idempotent by per-task `request_id`; commands also bind a complete LeaseKey and monotonically increasing revision. Repeating the same request reads its existing record; changing the action under that ID conflicts.

```text
Pending (intent committed)
  → Issued (accounted; same command may be sent/redelivered)
  → Succeeded / Failed (native observation accepted)
  → or Aborted (cancellation/termination ends unfinished control)
```

| Action | Success observation and lifecycle effect |
| --- | --- |
| `pause` | vCPUs paused; CPU charge becomes zero after accepted ACK |
| `offload` | Native offload succeeded; full RAM/slots remain reserved, without claiming zero residency |
| `resume` | Readmit and charge CPU before dispatch; ACK confirms Running |
| `checkpoint` | Full CPU/RAM/device/owned-filesystem state sealed; source execution continues |
| `suspend` | Seal full state and stop the frozen source; Suspending precedes Suspended after termination/delivery confirmation |

Unfinished controls must settle in order before conflicting actions. Workers validate lease/revision, persist observations and retry ACKs. Stale commands cannot control a new incarnation/generation. Cancellation or native termination ends outstanding controls.

For pause/offload observations and resource semantics, see [offload design](../offload/index.md) and [reservation handoffs](scheduling.md#reservations). Control failure does not optimistically reverse increased reservations.

## Attempt Gateway and inference waits {#inference}

The Worker creates an Attempt-local Gateway, matching task model capabilities and authorization. Model-provider credentials come from its host profile; Agents do not inherit Controller service credentials. Routing, capture and actual network boundaries are covered in [Gateway](../gateway.md) and [OverlayNet](../overlaynet.md).

A VM Worker can coordinate logical CPU release during inference when `[gateway]` enables both `enabled` and `release_cpu_on_idle`, and the Agent sends `x-pvisor-inference-idle: true`. This declares whole-guest idleness, including background work. A single HTTP request is not automatic proof of VM idleness. The local header is removed before upstream forwarding.

```text
Agent declares a quiescent call
  → Gateway creates a wait group (up to 64 cooperative concurrent calls)
  → begin → request Pause → native ACK → release CPU
  → dispatch model request
  → response reaches delivery point → ready → Resume admission
  → native Resume ACK + fresh lease confirmation
  → delivery_ready → Agent receives reply
```

Buffered replies wait for body EOF; SSE waits for its first nonempty chunk. Early HTTP headers do not trigger restoration. The group's first ready reply requests resume; subsequent ready replies do not pause a guest already processing a response.

`InferenceWaitKey` binds the lease, monotonic wait revision and authorized call ID; intents are `begin`, `ready` and `observe`. Each task retains one current wait and at most four automatic control receipts. Automatic controls do not consume the 4096-entry manual history, but share monotonic command revisions. Low-frequency transitions still consume log capacity; bounded in-memory history is not WAL compaction.

If Ready precedes an uncertain Begin, a Ready tombstone prevents a late Begin from pausing again. An unissued Pause can be aborted; an issued Pause must settle its ACK and pair with Resume. Restart preserves intent, but `observe` and other wait progress require a fresh Worker lease report.

Manual pause/offload/resume/checkpoint/suspend revokes automatic pause ownership. Automatic waits do not override human decisions. Cancellation, native termination and removal of the last wait member initiate cleanup bounded by execution lifetime; a terminal delivery lease cannot extend cleanup indefinitely. The `pvisor-inference-` request-ID prefix is reserved internally.

## Environments, checkpoints and restore {#checkpoint}

Environment templates combine architecture with ordered, pinned base/workspace/toolkit layers and are registered by immutable digest. Assignments carry fixed revision handles. Workers check support, read local or S3 lazy-cache layers and create independent uppers. Shared lowers do not imply a shared writable workspace.

A full execution checkpoint differs from an offload file. It seals CPU, RAM, devices and a complete owned-overlay inventory, validating original inode identities, open handles, directory cookies and other restore conditions. Continuation and fork establish lineage for new Run/Attempt identities. Missing, modified or incompatible snapshots are rejected.

Checkpoints remain in the source Worker's local execution storage. Restore and fork placement stays on the Worker owning that checkpoint, with independent writable trees and private COW RAM. Shared verified read-only lowers and RAM frames reduce duplication; sparse/compressed storage and native v5 lower pools remain executor mechanisms.

Restore verifies execution protocol, architecture, native format/profile and runtime environment. The source Worker's local storage must remain available. Cross-node checkpoint publication, import and restore placement are not provided.

## Sealed forks and live forks {#fork}

| Mode | Sequence | Failure boundary |
| --- | --- | --- |
| Sealed fork | Validate source full checkpoint/request → atomically create receipt and all branch tasks → independently admit/restore | Idempotent creation is not successful branch execution |
| Live fork | Commit request, capture control and WaitingCheckpoint branches → owning Worker captures/ACKs → atomically bind checkpoint and creation receipt → queue branches | Source completion, cancellation, expiry or capture failure ends branches still waiting for the checkpoint |

A request supports 1–64 branches, a 4 MiB transaction and the task-retention limit. Branches use distinct Task/Run/Attempt identities and preserve lineage. Each branch reserves resources separately; shared RAM is not a zero budget. A live-fork source may continue. Capture requests and ACKs bind source/key/revision; retry does not create duplicate branches.

Checkpoint acceptance, branch creation and cold restore are separate commit points. A branch-creation receipt does not prove native restore success. An unavailable source Worker or local checkpoint does not authorize placement on another Worker.

## Lifecycle and integration boundaries {#integration}

The ordinary Job checkpoint CLI and Cluster native restore are distinct product paths. Cluster support does not establish that all ordinary Job fork/restore paths are connected. RL integration must separately preserve rollout/scaffold state, model versions and rewards and coordinate GPU scheduling with environment restore; Cluster does not define training transactions.

Shared controls reside in `pvisor-core/src/cluster.rs`; manual controls/forks in `scheduler.rs`; automatic waits in `scheduler/inference.rs`; native execution, Gateway lifecycle, environment and snapshot integration in Worker `bin/worker/` modules. See the [validation matrix](operations.md#validation) for evidence boundaries.

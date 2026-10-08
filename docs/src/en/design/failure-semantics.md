# Failure semantics and retries

pVisor errors must be interpreted alongside their execution phase. Admission rejection, host I/O failure, lost receipts and caller cancellation leave different state. Before retrying, pin the original Job, Attempt, request/event ID and target, then distinguish rejection, accepted-but-unknown and completed work. Submitting a new ID bypasses the original reconciliation relationship.

## One execution has several commit points {#commit-boundaries}

![Execution moves from preparation and startup facts through dispatch, cleanup and publication](assets/execution-sequence.svg)

Session prepares file/network resources, commits required startup facts, then dispatches the executor. Cleanup, control observations, stage sealing, Bundle and terminal publication continue after executor exit. File apply, Journal append, snapshot publication and Job-head updates have separate boundaries. No transaction spans all of them and external services.

`Dispatched` therefore marks entry into dispatch, not proof that the guest started. A missing `Completed` also does not prove that a command had no effects. An HTTP peer may have processed a request or a file may have changed while the caller saw only a timeout. [Operation and Event](operations-events.md) owns fact contracts; the [Record matrix](records-and-versions.md) owns cross-record identities and versions.

## Choose recovery by failure location {#failure-matrix}

| Failure location | Possible retained state or effects | Reconcile | Retry boundary |
| --- | --- | --- | --- |
| Admission / driver preparation / startup fact commit | Workload not dispatched; preparation may have created directories, mounts or sockets | Session cleanup and preparation errors | Resolve resources/capabilities first; do not bypass failed startup-fact commit to dispatch |
| Post-execution recording or cleanup | Workload ran; upper changes or network effects may exist | Attempt, actual exit, Bundle/log and stage completeness | Do not rerun commands automatically to repair records |
| Host CLI timeout, disconnect or cancellation | Listener/worker may have accepted work and produced effects; cleanup may continue | Pinned Job state, artifacts and original request correlation | Host envelopes supply no generic deduplication; frontend does not automatically retry |
| Journal append returns Unknown | No bytes, a partial tail or a complete record may exist; handle is poisoned | Release clones/active writes, reopen and inspect the original Event ID | Retry append with identical Event ID and contents, without repeating external work |
| Uncertain execution capture / suspend / resume / fork receipt | Durable requests, objects, branches or successor Attempts may exist | Original request ID, Job head, current Attempt and exit/startup receipts | Reuse ID and parameters within that operation's scope; ambiguity may still return Unknown |
| Interrupted apply | Some target files changed; upper may not yet be pruned | Prepared / TargetApplied / Committed ledger and preimages under target lock | Recover the existing batch; do not delete the ledger and repeat everything |
| Incompletely sealed managed stage | Upper or observation logs may be incomplete | Writer shutdown, preimage completeness and seal | Reject apply/reuse; manually adding a seal cannot authenticate remnants |
| Failed VM freeze or mapping transition | CPU, devices and RAM may not form recoverable consistent state | Failed control transaction and runner ownership | Terminate the failed runner; unsupported rejections on healthy VMs follow their API contract |
| Daemon API process restart | Supervisors and VMs may remain alive | Persisted owner, process identity, credentials and actual supervisor | Reconcile ownership first; PID presence or absence alone cannot authorize adoption or recreation |

These describe failure semantics rather than a new shared error enum. Journal `Rejected`/`Unknown`, Operation Outcomes, typed Host errors and execution Job states belong to their own protocols. `Rejected` establishes only that the current action was not accepted at that interface; it cannot erase earlier business effects.

## Deduplication scope of each identity {#idempotency}

| Identity | Actual use | Repeated-submission constraint |
| --- | --- | --- |
| Event ID | Deduplication inside a Journal, returning the original position/receipt | Same ID requires identical serialized contents; changed timestamps or payloads conflict |
| Execution lifecycle request ID | Durable request/result correlation for the corresponding Job operation | Capture/suspend/resume/execution fork validate their bound parameters; no exactly-once claim for every command |
| Host envelope `request_id` | Correlates request, response, ticket, errors and cancellation | Does not give the transport a durable queue or generic deduplication |
| Checkpoint ID / manifest digest | Identifies saved state or content | Object existence does not prove source termination, Job-head commit or restore startup |

Journal retries retain the original Event object. Calling `Trace::event()` again generates a new UUID and therefore another event, even with identical prose. Job retries also retain original parameters. A new request ID denotes a new request and cannot safely probe whether the last one succeeded.

## Recovery does not mean redispatch {#reconciliation}

Suspend illustrates separate steps: accepting a request, publishing machine state, confirming source-runner exit and writing a suspended head. A matching Job/Attempt/request `ExecutionSuspension` terminal receipt supplies the corresponding termination proof. A snapshot directory or missing PID alone does not justify launching a second VM.

Resume first retains the restore request and destination Attempt path. If a repeated request finds uncertain successor records, it returns `EXECUTION_UNKNOWN` instead of launching another VM to obtain success. Error rollback is also conditional: the caller must still own that restore transition, with neither a live successor nor its `run.json`, before restoring previous Job state. Execution that occurred cannot automatically become “never ran”. [Job checkpoint design](job-checkpoint-cli.md#10-当前实现与验收边界) owns the detailed state machine.

Journal recovery repairs only an unfinished tail following a valid complete prefix. Complete corrupt lines, unsupported versions and causal cycles are rejected rather than deleted to manufacture success. Apply recovery instead reads durable intent and reconciles target state forward. External editors ignoring the target lock can still conflict. See [Journal recovery](journal.md#recovery) and [File apply recovery](overlayfs.md#apply-recovery).

## Canceling a wait, canceling execution and terminating resources {#cancellation}

Canceling a wait changes the observer. Journal appends accepted by the blocking pool may finish; `ManagedJobRun` retains Attempt join and completion publication for accepted durable Jobs. Frontend loss requests cancellation, but that request, process exit, device shutdown and resource cleanup are different facts. Continued runtime availability is also a prerequisite for asynchronous cleanup to finish.

Callers reconcile terminal state and cleanup before releasing stages, snapshot leases or shared-page references. Cancellation cannot retract bytes accepted by peers; file `drop` only disposes of candidate files. [Isolation mechanisms](isolation.md) defines executor process-control scope; [Freeze and restore](vm-runtime.md#freeze) defines VM failure-stop boundaries.

## Source map {#source-map}

| Entry, under `crates/` | Owned decisions |
| --- | --- |
| `pvisor/src/session.rs`, `session/lifecycle.rs` | Preparation, dispatch, cleanup and fact publication |
| `pvisor/src/runtime/job_service/managed.rs` | Accepted Attempt join, frontend cancellation and terminal publication |
| `pvisor-cli/src/cli/host_service.rs` | Listener, worker tickets, cancellation and request cleanup |
| `pvisor-journal/src/journal.rs` | Rejected/Unknown, poisoning, deduplication and reopen |
| `pvisor/src/runtime/job_execution.rs`, `runtime/job_service/lifecycle.rs`, `fork.rs` | Durable requests, restore and branch reconciliation |
| `pvisor-overlay-core/src/apply.rs`, `stage.rs` | Forward apply recovery and stage completeness |
| `pvisor-vm/src/handle.rs`, `vmm/mod.rs` | Control transactions, freeze failure and runner-disposition requirements |


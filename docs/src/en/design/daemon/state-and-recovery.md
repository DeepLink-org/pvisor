# Local state and restart recovery

Restart with the same private state directory and compatible runtime configuration to retain native ownership and unresolved reservations. A stored state is the last durable observation, not proof that a container survived.

## Authority and identity {#authority}

| State | Authority | Restart behavior |
| --- | --- | --- |
| Registry version and owner UUID | Durable daemon registry | Validate and retain the same owner |
| Sandbox identity, image/argv/env/metadata, resources, TTL and endpoint token | Durable record | Preserve intentions, access identity and conservative capacity |
| Running/Paused/Terminated | Native runtime observation | Inspect registered objects and update changed observations |
| `Stopping` | Durable deletion intent | Retry delete; inspection cannot overwrite it |
| Actual container and service readiness | Podman and prepared services | Registry contents cannot recreate them |

One daemon exclusively locks `daemon.lock`. Sandbox IDs use random UUIDs and are never reused. Every native operation checks owner/sandbox labels; registry ownership is local, not remote leader election or protection against the trusted host account.

## Startup reconciliation {#reconcile}

1. Open and validate the private registry, rejecting corrupt, oversized or incompatible contents rather than forgetting native ownership.
2. Construct the runtime with the persisted owner and complete native preflight before API binding.
3. Retry records with `Stopping`; inspect other registered sandboxes under their lifecycle lock.
4. Map native Running/Paused/Stopped/Missing to `Running`/`Paused`/`Terminated`/`Failed`. A missing sandbox remains visible and reserved until explicit deletion.
5. Report deferred reconciliation errors without inventing successful cleanup. Start maintenance to retry expired/pending deletion.

Normal shutdown leaves containers and records for restart. The list API returns last durable observations; GET reconciles native state. Maintenance is a deletion loop, not continuous native-process monitoring. Startup does not discover/adopt unknown containers or recreate missing workloads.

## Uncertainty and recovery {#failures}

| Failure | Preserved boundary | Action |
| --- | --- | --- |
| Create fails, cleanup confirmed | Record/reservation removed | Diagnose image/runtime before another request |
| Create cleanup uncertain | Failed record, ID in error, full reservation | Query/delete that ID; do not blindly resubmit |
| Native inspect unavailable | No fabricated native state | Repair runtime access and query again |
| Delete fails or absence unknown | `Stopping`, token/record and reservation retained | Restore runtime access and retry |
| Registry commit uncertain | Storage-failed latch refuses subsequent registry access | Preserve state, repair storage, restart and reconcile |
| Daemon down past TTL | No maintenance while down | Use host supervision if execution must end independently |

The registry is a local checkpoint, not a complete execution history or business-effect ledger. Restart does not roll back external calls, restore lost RAM or provide cross-host failover. Consistent automated backup/native-state restore is not implemented; copying a live directory or deleting state is not a recovery procedure.

The recovery path is implemented in `daemon/mod.rs`; registry validation and durability are in `daemon/store.rs`. See [storage](storage.md) and [operations](operations.md#runbook).

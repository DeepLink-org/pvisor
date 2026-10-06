# Local state and restart recovery

Restart with the same private state directory and compatible runtime configuration to retain native ownership and unresolved reservations. A stored state is the last durable observation, not proof that a VM survived.

## Authority and identity {#authority}

| State | Authority | Restart behavior |
| --- | --- | --- |
| Registry version and owner UUID | Durable daemon registry | Validate and retain the same owner |
| Sandbox identity, image/argv/env/metadata, resources, TTL and endpoint token | Durable record | Preserve intentions, access identity and conservative capacity |
| Running/Paused/Terminated | Native runtime observation | Inspect registered objects and update changed observations |
| `Stopping` | Durable deletion intent | Retry delete; inspection cannot overwrite it |
| Actual VM and service readiness | Native supervisor, VM controls, kernel cgroup and real services | Registry cannot recreate them |

One daemon exclusively locks `daemon.lock`. Private IPC authenticates same-UID peers plus owner, sandbox ID, generation and secret token; durable identity binds boot ID and cgroup device/inode. IDs are never reused or relaunched. Lost IPC is uncertainty, not Missing or cleanup proof. Durable deletion intent and the supervisor exclusive lock fence late launch; cleanup uses identity-bound `cgroup.kill`, never a persisted PID or PID-based kill, and confirms an empty cgroup plus released lock before capacity release. Replaced or missing same-boot cgroups without durable tombstone proof do not prove absence.

An occupied version-1 `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

A version-2 header without `owner.json` is rejected even though its `sandboxes` map is empty: that header activates private per-sandbox records, not an empty inventory. Restore original ownership state, never manufacture a marker. See [storage activation and migration](storage.md#migration).

## Startup reconciliation {#reconcile}

1. Open and validate the private registry, rejecting corrupt, oversized or incompatible contents rather than forgetting native ownership.
2. Construct the runtime with the persisted owner. Only after its factory accepts that owner, initialize/migrate storage: commit owner-wrapped records and metadata, then atomically activate the v2 header. Until activation the v1 full snapshot remains authoritative. Complete native preflight after storage initialization and before API binding.
3. Retry records with `Stopping`; inspect other registered sandboxes under their lifecycle lock.
4. Map native Running/Paused/Stopped/Missing to `Running`/`Paused`/`Terminated`/`Failed`. A missing sandbox remains visible and reserved until explicit deletion.
5. Report deferred reconciliation errors without inventing successful cleanup. Start maintenance to retry expired/pending deletion.

Normal daemon shutdown leaves detached supervisors/VMs and records. Restart reconnects private IPC, not a recreated VM or new Attempt; loss of IPC preserves uncertainty. GET reconciles while list returns durable observations. Startup does not adopt unknown VMs or relaunch missing/crashed workloads; host reboot cannot retain a live VM.

Cleanup validates durable owner/ID/generation and boot/cgroup bindings independently of launch resources: it does not require the original rootfs or firmware directory to remain present or guest argv/env/resource settings to pass launch validation. After proving native absence and acquiring the owner lock, it durably publishes `tombstone.json`, removes the empty owned cgroup and reclaims private run storage, live RAM backing, temporary overlays/specs, sockets and secret-bearing identity records and any legacy observation caches. A minimal directory retains the tombstone, owner lock and lifecycle markers to fence ID reuse. Interrupted reclamation resumes from the tombstone, including after cgroup removal; errors retain the reservation. Missing/replaced same-boot cgroups without this durable proof still do not authorize absence. See [storage](storage.md#gc).

## Uncertainty and recovery {#failures}

| Failure | Preserved boundary | Action |
| --- | --- | --- |
| Create fails, cleanup confirmed | Record/reservation removed | Diagnose image/runtime before another request |
| Create cleanup uncertain | Failed record, ID in error, full reservation | Query/delete that ID; do not blindly resubmit |
| Native inspect unavailable | No fabricated native state | Repair runtime access and query again |
| Delete fails or absence unknown | `Stopping`, token/record and reservation retained | Restore runtime access and retry |
| Registry commit uncertain | Storage-failed latch refuses subsequent registry access | Preserve state, repair storage, restart and reconcile |
| Daemon down past TTL | No maintenance while down | Use host supervision if execution must end independently |

The registry is an incrementally committed local inventory, not a complete execution history or business-effect ledger. Version 2 reconstructs it from the owner/header and validated per-sandbox records; it does not fall back to v1 when an activated records tree is corrupt or incomplete. Restart does not roll back external calls, restore lost RAM or provide cross-host failover. Consistent automated backup/native-state restore is not implemented; copying a live directory or deleting state is not a recovery procedure.

The recovery path is implemented in `daemon/mod.rs`; registry validation and durability are in `daemon/store.rs`. See [storage](storage.md) and [operations](operations.md#runbook).

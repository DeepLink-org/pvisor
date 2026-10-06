# Sandbox lifecycle

Confirm local native state before reporting a completed control or releasing resources. The daemon serializes operations per sandbox, while unrelated sandbox operations can progress independently.

## Creation and controls {#control}

| Operation | Durable/native sequence | Response |
| --- | --- | --- |
| Create | Persist `Pending` and reservation → create/start → verify configuration and service readiness → persist `Running` | 202 JSON |
| Pause | Inspect → persist `Pausing` → acknowledged vCPU pause → confirm Paused → persist `Paused` | 202 empty |
| Resume | Inspect → persist `Resuming` → acknowledged vCPU resume and readiness → confirm Running → persist `Running` | 202 empty |
| Delete | Persist `Stopping` → confirm native absence and exclusive owner lock → publish tombstone → reclaim cgroup/private run storage → confirm Missing → durably remove registry record | 204 |

An already observed desired pause/resume state can return success without another native control. Native errors cannot be treated as successful controls. Accepted lifecycle operations run in owned asynchronous tasks: an HTTP disconnect does not cancel them. A lost create response is not an idempotent retry contract; blindly resubmitting can create another sandbox.

Private IPC authenticates same-UID peers plus owner, sandbox ID, generation and secret token; durable identity binds boot ID and cgroup device/inode. IDs are never reused or relaunched. Lost IPC is uncertainty, not Missing or cleanup proof. Durable deletion intent and the supervisor exclusive lock fence late launch; cleanup uses identity-bound `cgroup.kill`, never a persisted PID or PID-based kill, and confirms an empty cgroup plus released lock before capacity release. Replaced or missing same-boot cgroups without durable tombstone proof do not prove absence.

Cleanup uses ownership bindings rather than image/launch validation; removed rootfs/firmware do not prevent it. A durable tombstone makes interrupted cgroup/private-storage reclamation retryable; only a minimal ID fence remains. Cleanup errors retain reservations. See [storage](storage.md#gc).

## Expiration {#expiration}

Omitted/null timeout means manual cleanup. A TTL is at least 60 seconds and within the configured maximum (one day by default, configuration ceiling one year). Renewal supplies a future RFC3339 deadline within the maximum; an existing deadline must extend, and expired or nonrenewable states are rejected.

Maintenance scans once per second and runs at most eight cleanup tasks concurrently. Cleanup rechecks expiration under the same lock as renewal, so a stale scan cannot delete a renewed sandbox. Pending deletion retries remain pending on failure.

TTL is best-effort cleanup, not a hard execution deadline. Slow native commands, upload/header waits and same-sandbox controls can delay deletion. New proxy requests are refused after expiration; established streams have no total response timeout. Daemon downtime suspends maintenance, so independent host supervision is needed for a stronger deadline.

## Service access and concurrency {#endpoints}

Only execd port 44772 and egress port 18080 are published. Endpoint discovery and each data-request endpoint lookup authenticate the live supervisor, require current RunHandle Running state, check deletion fences and resolve a current publication; they do not trust a durable observation cache. Discovery returns a daemon-routed authority, not the supervisor's loopback address. Lookup does not repeat service health probes or full cgroup-limit reconciliation for every data request. Create, Inspect and resume retain full configuration/readiness checks; upstream connection failures are handled by the API adapter rather than treated as readiness proof. Proxy establishment shares the lifecycle lock with control/deletion, preventing daemon-managed deletion/port reuse while connecting. External runtime manipulation is outside that coordination.

The proxy streams real upstream command/file/health/metrics traffic, including multipart and SSE. Upload plus response-header wait is capped at 120 seconds; established responses stream without a total timeout. WebSocket, CONNECT and arbitrary application ports are unsupported. Stream/output quotas belong to the upstream service.

## Separate execution semantics {#integration}

Native pause/resume controls live vCPUs through RunHandle on the same Attempt, not cgroup freeze, memory offload, checkpoint/restore or VM reconstruction. Admission charges remain. Execd forwarding is not Gateway routing, capture or inference-idle CPU release.

Ordinary Jobs retain their own [checkpoint/fork semantics](../execution-model.md). The daemon cannot stage/apply changes, restore native VM state, acquire node backing or coordinate model replies. Adding those paths would require an explicit adapter and lifecycle/evidence contract, not renaming retired distributed controls.

Lifecycle orchestration resides in `daemon/mod.rs`; native controls/readiness are in `runtime.rs`; streaming is in `daemon/api.rs`.

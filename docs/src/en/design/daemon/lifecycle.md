# Sandbox lifecycle

Confirm local native state before reporting a completed control or releasing resources. The daemon serializes operations per sandbox, while unrelated sandbox operations can progress independently.

## Creation and controls {#control}

| Operation | Durable/native sequence | Response |
| --- | --- | --- |
| Create | Persist `Pending` and reservation → create/start → verify configuration and service readiness → persist `Running` | 202 JSON |
| Pause | Inspect → persist `Pausing` → native freeze → confirm Paused → persist `Paused` | 202 empty |
| Resume | Inspect → persist `Resuming` → native unfreeze → confirm Running → persist `Running` | 202 empty |
| Delete | Persist `Stopping` → delete → confirm Missing → durably remove record | 204 |

An already observed desired pause/resume state can return success without another native control. Native errors cannot be treated as successful controls. Accepted lifecycle operations run in owned asynchronous tasks: an HTTP disconnect does not cancel them. A lost create response is not an idempotent retry contract; blindly resubmitting can create another sandbox.

Native operations verify owner/sandbox labels. Random sandbox IDs are not reused. Labels protect against accidental mismatched ownership, not malicious manipulation by the same host UID. The runtime's creation nonce also prevents failed-create cleanup from removing a different creation attempt.

## Expiration {#expiration}

Omitted/null timeout means manual cleanup. A TTL is at least 60 seconds and within the configured maximum (one day by default, configuration ceiling one year). Renewal supplies a future RFC3339 deadline within the maximum; an existing deadline must extend, and expired or nonrenewable states are rejected.

Maintenance scans once per second and runs at most eight cleanup tasks concurrently. Cleanup rechecks expiration under the same lock as renewal, so a stale scan cannot delete a renewed sandbox. Pending deletion retries remain pending on failure.

TTL is best-effort cleanup, not a hard execution deadline. Slow native commands, upload/header waits and same-sandbox controls can delay deletion. New proxy requests are refused after expiration; established streams have no total response timeout. Daemon downtime suspends maintenance, so independent host supervision is needed for a stronger deadline.

## Service access and concurrency {#endpoints}

Only execd port 44772 and egress port 18080 are published. Endpoint discovery checks an actual native publication and returns a daemon-routed authority, not the container's loopback address. Proxy establishment shares the lifecycle lock with control/deletion, preventing daemon-managed deletion/port reuse while connecting. External runtime manipulation is outside that coordination.

The proxy streams real upstream command/file/health/metrics traffic, including multipart and SSE. Upload plus response-header wait is capped at 120 seconds; established responses stream without a total timeout. WebSocket, CONNECT and arbitrary application ports are unsupported. Stream/output quotas belong to the upstream service.

## Separate execution semantics {#integration}

Podman pause is cgroup freeze, not VM suspension, memory offload or a checkpoint. It retains all admission charges. Execd forwarding is not pVisor Gateway model routing, capture or inference-idle CPU release.

Ordinary Jobs retain their own [checkpoint/fork semantics](../execution-model.md). The daemon cannot stage/apply changes, restore native VM state, acquire node backing or coordinate model replies. Adding those paths would require an explicit adapter and lifecycle/evidence contract, not renaming retired distributed controls.

Lifecycle orchestration resides in `daemon/mod.rs`; native controls/readiness are in `runtime.rs`; streaming is in `daemon/api.rs`.

# Operate the single-node daemon

Keep the same private state directory and unchanged absolute native runtime paths/cgroup identity across restarts. The daemon owns local sandbox records and deletion intentions; it does not recover distributed tasks or replay a global DAG.

## Inspect and delete {#lifecycle}

Use the authenticated lifecycle API started in the [installation guide](index.md#start):

| Request | Meaning |
| --- | --- |
| `GET /v1/sandboxes` | Last durable observations; supports repeated state filters, SDK-encoded metadata and page/pageSize |
| `GET /v1/sandboxes/{id}` | Reconcile this sandbox with local native state |
| `DELETE /v1/sandboxes/{id}` | Confirm native deletion before releasing reservation; success returns 204 |
| `POST /v1/sandboxes/{id}/pause` | Confirm acknowledged live vCPU pause; 202 with empty body |
| `POST /v1/sandboxes/{id}/resume` | Confirm acknowledged live vCPU resume; 202 with empty body |
| `POST /v1/sandboxes/{id}/renew-expiration` | Extend an existing TTL using `expiresAt`, a future RFC3339 deadline |

Replace `{id}` with the sandbox ID returned by creation/listing. HTTP disconnects do not cancel accepted operations. Different sandboxes have independent lifecycle locks; controls and connection establishment for the same sandbox serialize.

Do not blindly repeat a create request after losing its response: this API is not the old task-ID idempotency protocol. Inspect retained state first. If creation reports an uncertain native outcome, retain the sandbox ID in the error message and inspect/delete it. Verified cleanup of failed creation releases admission; uncertain cleanup retains the record and reservation.

## Endpoint authentication {#endpoints}

Resolve `GET /v1/sandboxes/{id}/endpoints/44772` for execd or port `18080` for egress. Returned authorities have no URL scheme and route through the daemon, not directly to supervisor loopback publications.

- With `use_server_proxy=true`, authenticate routed requests using the lifecycle `OPEN-SANDBOX-API-KEY` header.
- Default mode supplies a random sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` in endpoint `headers`. Clients must preserve those headers; a token does not authorize lifecycle operations or another sandbox.
- Control credentials are stripped before forwarding to the guest. Guest execd access tokens, when supplied, are not replaced by the lifecycle key.

Signed endpoint expiration, arbitrary application ports, WebSocket and CONNECT are unsupported. Streaming requests/responses include multipart and SSE. Upload plus upstream-header wait is capped at 120 seconds; established response streams have no total timeout. Output/stream quotas remain the upstream service's responsibility.

## Expiration and shutdown {#expiration}

Creation `timeout` is optional; omitted/null means manual cleanup. A configured TTL is at least 60 seconds and no greater than `--max-timeout-seconds` (default one day, configurable up to one year). Renewal must extend an existing deadline.

Expiration cleanup is **best effort**, not a hard execution deadline. Native commands, uploads/header waits and same-sandbox controls can delay deletion. New proxy requests are rejected after expiration. Renewal and expiration checks share the lifecycle lock, so a stale scan cannot delete a sandbox whose deadline was extended.

Normal daemon shutdown leaves native sandboxes and durable records for restart. Maintenance resumes on restart; there is no cleanup guarantee while the daemon is down. For a hard deadline or cleanup during downtime, configure native host supervision. Delete sandboxes explicitly before retiring a deployment, confirm native cleanup, then stop its daemon.

## Restart and state ownership {#restart}

One daemon exclusively locks private state; owner identity persists before native creation/control. Private IPC authenticates same-UID peers plus owner, sandbox ID, generation and secret token; durable identity binds boot ID and cgroup device/inode. IDs are never reused or relaunched. Lost IPC is uncertainty, not Missing or cleanup proof. Durable deletion intent and the supervisor exclusive lock fence late launch; cleanup uses identity-bound `cgroup.kill`, never a persisted PID or PID-based kill, and confirms an empty cgroup plus released lock before capacity release. Replaced or missing same-boot cgroups without durable tombstone proof do not prove absence.

Pending deletion survives restart and cannot be overwritten by a native observation. Unknown deletion retains its reservation. Missing native sandboxes remain visible as Failed until explicitly deleted. The maintenance loop retries expired/pending deletion; list is not continuous native process monitoring, so use GET to reconcile a specific sandbox.

Never delete state to fix an error or run two daemons against it. State carries native ownership and unresolved reservations. Moving old Controller journals into this directory does not migrate history. The bounded registry has configured capacity and a 16 MiB limit; it is not an unbounded distributed task archive or a Run Bundle/artifact store.

Delete/reconciliation can finish tombstone-backed cleanup without the original rootfs or firmware. It reclaims the empty owned cgroup and private run/RAM/temp/socket/secret records, retaining a minimal ID fence. Reclamation errors preserve reservations for retry; do not remove tombstones or registry state yourself. See [storage](../../design/daemon/storage.md#gc).

An occupied `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

## Troubleshooting {#troubleshooting}

| Symptom | Check |
| --- | --- |
| Startup fails before bind | Linux x86_64/KVM, short absolute state, trusted manifests and real delegated cgroup v2 |
| Occupied registry lacks native `owner.json` | Preserve old state/containers; use fresh native state or clean up all sandboxes through the old Podman daemon before switching |
| State directory is busy | Another daemon owns its exclusive lock; do not remove the lock/state to bypass it |
| Image unavailable | Provision the prepared image locally; automatic pull and image auth are unsupported |
| SDK creation never becomes ready | Real execd initialization and `/ping`/`/ready`, egress `/healthz`, and guest CID 3 vsock bridges on 44772/18080; stock rootfs is insufficient |
| Endpoint is unreachable externally | TLS proxy routing, domain/protocol, `--public-endpoint` and endpoint-provided headers |
| Admission remains full after failure/pause | Failed/uncertain/paused sandboxes retain hard reservations; inspect and confirm deletion |
| Expired sandbox still exists | Maintenance is best effort; daemon uptime, lifecycle locks and native cleanup errors |
| Requested policy/template/volume is refused | Unsupported options are rejected, not silently installed or simulated |

API errors contain `{code, message}` and responses include `X-Request-ID`. Preserve status, request ID and sanitized diagnostics; API success is not a command exit status. See [exit codes and errors](../../reference/exit-codes.md#daemon).

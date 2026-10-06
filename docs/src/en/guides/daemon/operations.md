# Operate the single-node daemon

Keep the same private state directory and compatible Podman configuration across restarts. The daemon owns local sandbox records and deletion intentions; it does not recover distributed tasks or replay a global DAG.

## Inspect and delete {#lifecycle}

Use the authenticated lifecycle API started in the [installation guide](index.md#start):

| Request | Meaning |
| --- | --- |
| `GET /v1/sandboxes` | Last durable observations; supports repeated state filters, SDK-encoded metadata and page/pageSize |
| `GET /v1/sandboxes/{id}` | Reconcile this sandbox with local native state |
| `DELETE /v1/sandboxes/{id}` | Confirm native deletion before releasing reservation; success returns 204 |
| `POST /v1/sandboxes/{id}/pause` | Confirm native cgroup freeze; 202 with empty body |
| `POST /v1/sandboxes/{id}/resume` | Confirm native cgroup unfreeze; 202 with empty body |
| `POST /v1/sandboxes/{id}/renew-expiration` | Extend an existing TTL using `expiresAt`, a future RFC3339 deadline |

Replace `{id}` with the sandbox ID returned by creation/listing. HTTP disconnects do not cancel accepted operations. Different sandboxes have independent lifecycle locks; controls and connection establishment for the same sandbox serialize.

Do not blindly repeat a create request after losing its response: this API is not the old task-ID idempotency protocol. Inspect retained state first. If creation reports an uncertain native outcome, retain the sandbox ID in the error message and inspect/delete it. Verified cleanup of failed creation releases admission; uncertain cleanup retains the record and reservation.

## Endpoint authentication {#endpoints}

Resolve `GET /v1/sandboxes/{id}/endpoints/44772` for execd or port `18080` for egress. Returned authorities have no URL scheme and route through the daemon, not directly to container loopback ports.

- With `use_server_proxy=true`, authenticate routed requests using the lifecycle `OPEN-SANDBOX-API-KEY` header.
- Default mode supplies a random sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` in endpoint `headers`. Clients must preserve those headers; a token does not authorize lifecycle operations or another sandbox.
- Control credentials are stripped before forwarding to the guest. Guest execd access tokens, when supplied, are not replaced by the lifecycle key.

Signed endpoint expiration, arbitrary application ports, WebSocket and CONNECT are unsupported. Streaming requests/responses include multipart and SSE. Upload plus upstream-header wait is capped at 120 seconds; established response streams have no total timeout. Output/stream quotas remain the upstream service's responsibility.

## Expiration and shutdown {#expiration}

Creation `timeout` is optional; omitted/null means manual cleanup. A configured TTL is at least 60 seconds and no greater than `--max-timeout-seconds` (default one day, configurable up to one year). Renewal must extend an existing deadline.

Expiration cleanup is **best effort**, not a hard execution deadline. Native commands, uploads/header waits and same-sandbox controls can delay deletion. New proxy requests are rejected after expiration. Renewal and expiration checks share the lifecycle lock, so a stale scan cannot delete a sandbox whose deadline was extended.

Normal daemon shutdown leaves native sandboxes and durable records for restart. Maintenance resumes on restart; there is no cleanup guarantee while the daemon is down. For a hard deadline or cleanup during downtime, configure native host supervision. Delete sandboxes explicitly before retiring a deployment, confirm native cleanup, then stop its daemon.

## Restart and state ownership {#restart}

One daemon exclusively locks its private state directory. Native owner identity and records persist before creation/control. Native operations validate owner/sandbox labels, and random sandbox IDs are not reused. Labels do not defend against another process with the same host UID.

Pending deletion survives restart and cannot be overwritten by a native observation. Unknown deletion retains its reservation. Missing native sandboxes remain visible as Failed until explicitly deleted. The maintenance loop retries expired/pending deletion; list is not continuous native process monitoring, so use GET to reconcile a specific sandbox.

Never delete state to fix an error or run two daemons against it. State carries native ownership and unresolved reservations. Moving old Controller journals into this directory does not migrate history. The bounded registry has configured capacity and a 16 MiB limit; it is not an unbounded distributed task archive or a Run Bundle/artifact store.

## Troubleshooting {#troubleshooting}

| Symptom | Check |
| --- | --- |
| Startup fails before bind | Linux, trusted absolute Podman path, rootless operation, cgroup v2 and delegated CPU/memory/PID controllers |
| State directory is busy | Another daemon owns its exclusive lock; do not remove the lock/state to bypass it |
| Image unavailable | Provision the prepared image locally; automatic pull and image auth are unsupported |
| SDK creation never becomes ready | Real execd initialization and `/ping`/`/ready`, egress `/healthz`, and capability-free egress; stock images are insufficient |
| Endpoint is unreachable externally | TLS proxy routing, domain/protocol, `--public-endpoint` and endpoint-provided headers |
| Admission remains full after failure/pause | Failed/uncertain/paused sandboxes retain hard reservations; inspect and confirm deletion |
| Expired sandbox still exists | Maintenance is best effort; daemon uptime, lifecycle locks and native cleanup errors |
| Requested policy/template/volume is refused | Unsupported options are rejected, not silently installed or simulated |

API errors contain `{code, message}` and responses include `X-Request-ID`. Preserve status, request ID and sanitized diagnostics; API success is not a command exit status. See [exit codes and errors](../../reference/exit-codes.md#daemon).

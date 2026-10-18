# Daemon operations

Run a single-node sandbox service with the native VM runtime, private state and explicit resource capacity. Invoke `pvisor-daemon` directly. It owns sandbox services and its optional detached pool; `pvisor-cache` remains independent.

## Deployment {#deployment}

Platform prerequisites (Linux x86_64, `/dev/kvm`, a delegated cgroup v2 hierarchy) and how they are checked are documented in the [operations guide's prerequisites](../../guides/daemon/index.md#prerequisites).

Set `OPEN_SANDBOX_API_KEY` to a protected random secret of at least 32 bytes, not argv.

`serve` constructs `NativeRuntime` using the implemented, required `--images-dir` and `--cgroup-root` flags. Cargo links `pvisor` and `pvisor-core`; synchronous `main` calls `pvisor::run_krun_internal_if_requested()` before argument parsing or Tokio, then dispatches the hidden `native-supervisor --sandbox-dir ABSOLUTE_PATH` command. The deployment example below uses the current CLI, but does not supply or validate the guest bootstrap, SDK conformance or density.

```sh
pvisor-daemon protocol
pvisor-daemon serve \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

Keep state and runtime configuration compatible across restarts. Use TLS termination in a trusted reverse proxy before non-loopback exposure. Set `--public-endpoint` to the externally routed host:port authority; wildcard or ephemeral listeners require it. Automatic endpoint publication is not implemented.

Runtime/state path absoluteness, length and persistence constraints are documented in the [operations guide's install and start](../../guides/daemon/index.md#install).

An occupied v1 registry or a v2 header without the native `owner.json` marker is rejected, even with an intentionally empty map: it may still own live Podman containers. Preserve the original ownership state; use fresh native state or clean up through the old Podman daemon before switching, and never erase records/reservations or fabricate markers. Rejection and migration rules live in [storage](storage.md#migration).

## Prepared image contract {#image-contract}

Image manifest fields and environment semantics are documented in the [operations guide's prerequisites](../../guides/daemon/index.md#prerequisites).

The long-lived guest bootstrap must supervise workload, real OpenSandbox 1.1.0 execd and egress, initialize/authenticate services, forward signals and reap children. It must expose byte-transparent AF_VSOCK listeners on guest **CID 3**, ports **44772/18080**, bridging to real services. Supervisor loopback TCP publications connect through private Unix sockets and native vsock forwarding. A stock rootfs or sleeping process is insufficient.

Create, Inspect of a Running VM and resume readiness checks require genuine HTTP 200 execd `/ping`, `/ready` with JSON `initialized: true`, and egress `/healthz` through the bridges, with bounded bodies. Endpoint lookup authenticates live Running state and deletion fences without repeating full health/cgroup checks per data request. The daemon does not inject/initialize execd or synthesize command/SSE/file responses. Python SDK initialization resolves both endpoints even without network policy.

The bootstrap and image recipe are **not supplied or end-to-end validated**. The old container `cap-drop=ALL` restriction does not describe this native VM backend; an upstream image name is not a native bootstrap/vsock adapter. No fake readiness, SDK-conformance or density evidence is provided.

## Partial protocol profile {#api}

Baseline: OpenSandbox **1.1.0**, `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`; upgrades require explicit schema/SDK/test review.

| Interface | Current behavior |
| --- | --- |
| `POST /v1/sandboxes` | Image/argv/env/metadata, hard CPU/memory, optional TTL; 202 JSON |
| `GET /v1/sandboxes` | Repeated states, SDK-encoded metadata, page/pageSize; last durable observations |
| `GET /v1/sandboxes/{id}` | Reconcile native state; 200 JSON |
| `DELETE /v1/sandboxes/{id}` | Confirm deletion before release; 204 |
| `POST /v1/sandboxes/{id}/pause`, `/resume` | Confirm live vCPU pause/resume on the same Attempt; 202 empty |
| `POST /v1/sandboxes/{id}/renew-expiration` | Future RFC3339 deadline extending any existing TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | Actual execd/egress publication through daemon authority |
| Data plane | Stream prepared services; do not reimplement their APIs |

Lifecycle requests use `OPEN-SANDBOX-API-KEY`. Responses carry `X-Request-ID`; errors use `{code, message}`. Server-proxy endpoints use the lifecycle key; default endpoints provide sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` headers that SDKs must preserve. Tokens cannot control lifecycle or access another sandbox. Control secrets are stripped upstream, but caller-supplied execd access credentials are not replaced.

Snapshots, templates/pools, metadata mutation, hooks, network policy, credential proxy, secure access, volumes, image auth, arbitrary ports, signed endpoints, WebSocket and CONNECT are unsupported. Native VM execution is wired in the runtime; stage/apply, checkpoint/fork and offload APIs are not implemented.

## Trust boundary {#security}

Native OverlayNet supplies VM outbound networking; OpenSandbox network-policy requests remain unsupported and rejected, not deny-all egress. Trust the host account, daemon/firmware and prepared image; private state and same-UID IPC do not protect against hostile host-UID/root code. Other local users may reach loopback publications, so real service authentication and host controls remain necessary. Secrets stay out of supervisor argv and host environment but persist in private records. No security audit or hostile multi-user assurance is claimed.

## Failure handling {#runbook}

| Symptom | Action |
| --- | --- |
| Lost create/control response | Inspect existing records/effects; disconnect does not cancel accepted work and create has no retry-idempotency key |
| Capacity exhausted | Inspect retained records; pause or low RSS does not release reservations |
| Failed/missing native sandbox | Preserve record, diagnose runtime, explicitly delete to release resources |
| Pending delete/TTL cleanup | Restore runtime access; maintenance retries, but TTL is not a hard deadline |
| Storage commit uncertain/corrupt registry | Preserve directory, repair storage and restart; never erase ownership state to proceed |
| Daemon shutdown | Detached native supervisors/VMs survive daemon-only restart; restart with the same owner/state, or arrange independent host cleanup |
| Readiness/endpoint failure | Verify real services and guest bootstrap/vsock bridges, not just a Running VM |

## Evidence scope {#validation}

Source tests cover fake-runtime admission/lifecycle/restart/TTL, HTTP auth/schemas/filtering/endpoints and pure runtime parsing/argv. They do not establish native isolation, SDK end-to-end compatibility or density gains. No build, test, native sandbox or benchmark validates the current daemon. Prior distributed Cluster gates and measurements do not validate this daemon.

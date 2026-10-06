# Daemon operations

Run a single-node sandbox service with a trusted absolute Podman path, a private persistent state directory and explicit resource capacity. Invoke `pvisor-daemon` directly; proposed CLI companion wiring is not a deployment prerequisite.

## Deployment {#deployment}

Requires Linux, external rootless Podman, cgroup v2 with delegated CPU/memory/PID controllers, and locally provisioned prepared images. Set `OPEN_SANDBOX_API_KEY` to a strong random secret of at least 32 bytes through your secret mechanism; avoid argv.

The following is a deployment example, **not a tested installation or end-to-end SDK recipe**:

```sh
pvisor-daemon protocol
pvisor-daemon serve \
  --podman /usr/bin/podman \
  --listen 127.0.0.1:8080 \
  --state /var/tmp/pvisor-daemon-state \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

Keep state and runtime configuration compatible across restarts. Use TLS termination in a trusted reverse proxy before non-loopback exposure. Set `--public-endpoint` to the externally routed host:port authority; wildcard or ephemeral listeners require it. Automatic endpoint publication is not implemented.

Defaults: listen `127.0.0.1:8080`, state `.pvisor/daemon`, 32 sandboxes, 4000 CPU millis, 8 GiB admitted memory and maximum TTL 86400 seconds. Capacity is not a complete service-group resource cap.

## Prepared image contract {#image-contract}

Images are provisioned locally and creation uses `--pull=never`. Their ENTRYPOINT must supervise requested argv (which replaces CMD), forward signals and reap children without shell interpolation. It must also run real **OpenSandbox 1.1.0 execd** on 44772 and an egress service on 18080, completing initialization and service authentication itself.

Creation probes execd `/ping`, `/ready` and egress `/healthz`. The daemon does not inject `/execd`, perform its initialization handshake or emulate command/SSE/file responses. A stock image running `tail` is not an SDK sandbox.

The pinned upstream default egress uses iptables redirects and cannot run unchanged with `cap-drop=ALL`. A genuine capability-free deployment is required; the ordinary sidecar is unsupported. No end-to-end prepared-image recipe has been validated. Ordinary SDK creation/readiness fails if the contract is unmet, rather than returning a simulated ready sandbox. Python SDK initialization resolves both endpoints even without a requested network policy.

## Partial protocol profile {#api}

Baseline: OpenSandbox **1.1.0**, `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`; upgrades require explicit schema/SDK/test review.

| Interface | Current behavior |
| --- | --- |
| `POST /v1/sandboxes` | Image/argv/env/metadata, hard CPU/memory, optional TTL; 202 JSON |
| `GET /v1/sandboxes` | Repeated states, SDK-encoded metadata, page/pageSize; last durable observations |
| `GET /v1/sandboxes/{id}` | Reconcile native state; 200 JSON |
| `DELETE /v1/sandboxes/{id}` | Confirm deletion before release; 204 |
| `POST /v1/sandboxes/{id}/pause`, `/resume` | Confirm native freeze/unfreeze; 202 empty |
| `POST /v1/sandboxes/{id}/renew-expiration` | Future RFC3339 deadline extending any existing TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | Actual execd/egress publication through daemon authority |
| Data plane | Stream prepared services; do not reimplement their APIs |

Lifecycle requests use `OPEN-SANDBOX-API-KEY`. Responses carry `X-Request-ID`; errors use `{code, message}`. Server-proxy endpoints use the lifecycle key; default endpoints provide sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` headers that SDKs must preserve. Tokens cannot control lifecycle or access another sandbox. Control secrets are stripped upstream, but caller-supplied execd access credentials are not replaced.

Snapshots, templates/pools, metadata mutation, hooks, network policy, credential proxy, secure access, volumes, image auth, arbitrary ports, signed endpoints, WebSocket and CONNECT are unsupported. Native VM/stage/offload capabilities are not added by protocol compatibility.

## Trust boundary {#security}

Rootless containers share the host kernel; there is no VM-grade boundary or completed security audit. Trust the host account, Podman configuration/hooks and prepared image. Slirp4netns disables host-loopback access but is not deny-all egress; requested network policies are rejected. Native loopback ports may remain accessible to other local users, requiring real service authentication and host controls for multi-user exposure.

## Failure handling {#runbook}

| Symptom | Action |
| --- | --- |
| Lost create/control response | Inspect existing records/effects; disconnect does not cancel accepted work and create has no retry-idempotency key |
| Capacity exhausted | Inspect retained records; pause or low RSS does not release reservations |
| Failed/missing native sandbox | Preserve record, diagnose runtime, explicitly delete to release resources |
| Pending delete/TTL cleanup | Restore runtime access; maintenance retries, but TTL is not a hard deadline |
| Storage commit uncertain/corrupt registry | Preserve directory, repair storage and restart; never erase ownership state to proceed |
| Daemon shutdown | Containers survive; restart with the same owner/state, or arrange independent host cleanup |
| Readiness/endpoint failure | Verify real services and the capability-free egress contract, not just container Running |

## Evidence scope {#validation}

Source tests cover fake-runtime admission/lifecycle/restart/TTL, HTTP auth/schemas/filtering/endpoints and pure runtime parsing/argv. They do not establish native isolation, SDK end-to-end compatibility or density gains. No build, test, native sandbox or benchmark was run for this documentation change. Prior distributed Cluster gates and measurements do not validate this daemon.

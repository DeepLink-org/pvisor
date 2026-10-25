# Daemon operations

Run a single-node sandbox service with the native VM runtime, private state and explicit resource capacity. Invoke `pvisor-daemon` directly. It owns sandbox services and its optional detached pool; `pvisor cache` provides cache client commands, and `pvisor-daemon serve --cache` serves the cache.

## Deployment {#deployment}

Platform prerequisites (Linux x86_64, `/dev/kvm`, a delegated cgroup v2 hierarchy) and how they are checked are documented in the [operations guide's prerequisites](../../guides/daemon/index.md#prerequisites).

Set `OPEN_SANDBOX_API_KEY` to a protected random secret of at least 32 bytes, not argv.

`serve --sandbox` constructs `NativeRuntime` from `--images-dir` and `--cgroup-root`. It starts the runtime in independent supervisors, with internal VM dispatch before Tokio. Prepare images and the API key using the [user guide](../../guides/daemon/index.md#install), then start serving:

```sh
pvisor-daemon protocol
pvisor-daemon serve --sandbox \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

Keep state and runtime configuration compatible across restarts. Use TLS termination in a trusted reverse proxy before non-loopback exposure. Set `--public-endpoint` to the externally routed host:port authority; wildcard listeners require it. Ephemeral loopback listeners publish their allocated address; non-loopback binding is rejected unless explicitly enabled with `--allow-insecure-listen` for remote plaintext access.

Runtime/state path absoluteness, length and persistence constraints are documented in the [operations guide's install and start](../../guides/daemon/index.md#install).

An occupied v1 registry or a v2 header without the native `owner.json` marker is rejected, even with an intentionally empty map: it may still own live Podman containers. Preserve the original ownership state; use fresh native state or clean up through the old Podman daemon before switching, and never erase records/reservations or fabricate markers. Rejection and migration rules live in [storage](storage.md#migration).

## Prepared image contract {#image-contract}

`prepare-image` prepares a registry image in the shared compressed pool, pins its revision and atomically publishes a local template manifest. Create opens that revision's lazy projection and retains its owner until VM cleanup completes. Registry pulls and complete extraction are outside creation. See the [user guide](../../guides/daemon/index.md#install) for keys, preparation commands and SDK examples.

New templates default to `native_guest: true`. The `pvisor-guest` PID 1, process groups and reaper own workloads. Host `native/opensandbox.rs` or `native/e2b.rs` adapts HTTP/SSE or JSON/Connect and invokes the guest process/file engine over host-CID-only vsock 1025. Readiness binds sandbox/generation; the main entrypoint's exit ends the sandbox. There is no host-execution fallback.

Explicit `--execd` or `--envd` retains the upstream compatibility backend. Preparation validates static Linux ELF binaries and injects a managed bootstrap; a private handshake initializes genuine services and checks binding, authentication and readiness. Legacy standalone-rootfs manifests are not converted: their bootstrap must keep supplying the corresponding guest services and vsock bridges. See the [crate contract](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md#runtime-boundary-native-vm-and-prepared-images) for formats and legacy requirements.

Templates and host code are trusted inputs. ELF/digest checks do not authenticate third-party provenance. Failed preparation preserves the previous manifest. If rename or directory-sync results are uncertain, inspect published state before retrying.

## Partial protocol profile {#api}

OpenSandbox baseline: **1.1.0**, `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`; upgrades require explicit schema/SDK/test review.

| Interface | Current behavior |
| --- | --- |
| `POST /v1/sandboxes` | Image/argv/env/metadata, hard CPU/memory, optional TTL; 202 JSON |
| `GET /v1/sandboxes` | Repeated states, SDK-encoded metadata, page/pageSize; last durable observations |
| `GET /v1/sandboxes/{id}` | Reconcile native state; 200 JSON |
| `DELETE /v1/sandboxes/{id}` | Confirm deletion before release; 204 |
| `POST /v1/sandboxes/{id}/pause`, `/resume` | Confirm live vCPU pause/resume on the same Attempt; 202 empty |
| `POST /v1/sandboxes/{id}/renew-expiration` | Future RFC3339 deadline extending any existing TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | Actual execd/egress publication through daemon authority |
| Data plane | Guest engine and host protocol adapters by default; proxy for explicit upstream templates |

Lifecycle requests use `OPEN-SANDBOX-API-KEY`. Responses carry `X-Request-ID`; errors use `{code, message}`. Server-proxy endpoints use the lifecycle key; default endpoints provide sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` headers that SDKs must preserve. Tokens cannot control lifecycle or access another sandbox. Control secrets are stripped upstream, but caller-supplied execd access credentials are not replaced.

Snapshots, templates/pools, metadata mutation, hooks, network policy, credential proxy, secure access, volumes, image auth, arbitrary ports, signed endpoints, WebSocket and CONNECT are unsupported. Native VM execution is wired in the runtime; stage/apply, checkpoint/fork and offload APIs are not implemented.

The same service also supports a partial local E2B Python SDK **2.59.0** profile through separate `/sandboxes`, `/v2/sandboxes` and `/e2b/*` routes. See [E2B access](../../guides/daemon/index.md#e2b) for protocol/authentication setup. Local `prepare-image` preparation and SDK template-build APIs are separate interfaces.

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
| Readiness/endpoint failure | Check guest binding, entrypoint and authentication for the selected backend; explicit upstream templates also require service initialization and bridges |

## Evidence scope {#validation}

Conventional Rust tests cover admission, lifecycle, restart, TTL, authentication, schemas, state transitions, native IPC/cgroup identity and cleanup. Official SDK checks against real VMs cover OpenSandbox 1.1.0 and E2B Python 2.59.0 creation, concurrent sandboxes, commands, files, pause/resume, authentication and deletion. A separate restart check verifies the same VM, files and live processes across API restart.

Acceptance entry points are `crates/pvisor-daemon/tests/opensandbox_sdk.py`, `e2b_sdk.py` and `native_sdk_restart.py`. They require an isolated daemon, prepared templates and real KVM/cgroup prerequisites; see the [crate test instructions](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md#validation). Ordinary `mise run test -- pvisor-daemon` does not automatically run these SDK drivers.

Results apply to the listed core profiles. Full SDK conformance, production density, network policies and hostile multi-user security require separate validation. Historical Cluster measurements cannot substitute for current daemon evidence.

# Run the single-node daemon

Install `pvisor-daemon` to manage image-based sandboxes on one Linux host through a partial OpenSandbox 1.1.0 API. It owns local admission, lifecycle, durable state and expiration. It does not schedule business tasks across nodes or provide a global DAG, distributed leases, Controller or Workers. Use Kubernetes, Ray or your application for orchestration.

| Need | Guide |
| --- | --- |
| Install and start the API | The commands below |
| Prepare images and choose an execution boundary | [Runtime and integration boundaries](boundaries.md) |
| Inspect, delete and recover sandboxes | [Operations](operations.md) |
| Keep native node/cache/pool services separate | [Service entry points](service.md) |

## Prerequisites {#prerequisites}

Use Linux with rootless Podman, cgroup v2 and delegated CPU, memory and PID controllers. The daemon requires a trusted absolute Podman executable path and checks the runtime before binding the API. It fails rather than falling back to host execution or uncapped containers.

A working sandbox also requires a **locally preprovisioned image** with real OpenSandbox 1.1.0 execd and an egress service. The daemon never pulls images. A stock distribution image, a sleeping container or an upstream image name alone does not satisfy this contract.

!!! warning
    The pinned upstream default egress component installs iptables redirects and cannot run unchanged with this backend's `cap-drop=ALL`. No end-to-end prepared-image recipe has been validated. You can install and start the lifecycle API, but do not expect ordinary SDK `Sandbox.create()` readiness until you have a genuine capability-free execd/egress deployment. See the [image contract](boundaries.md#images).

## Install the executable {#install}

From a checkout of the revision you intend to deploy, with Rust/Cargo installed:

```bash
cargo install --locked --path crates/pvisor-daemon --bin pvisor-daemon
pvisor-daemon --help
pvisor-daemon protocol
```

`protocol` prints the pinned OpenSandbox version and commit; it does not certify full SDK compatibility. This source installation is separate from installing the Python `pvisor` package. Do not assume an existing wheel includes the new daemon companion or prepared images. Pin the source revision, SDK 1.1.0 and image contents together.

## Start the API {#start}

Choose a private persistent state directory outside the checkout. Run as the non-root account that owns the prepared Podman images. Generate a secret once, retain it in your service's protected secret storage, and use the same value on restart:

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve \
  --podman /usr/bin/podman \
  --listen 127.0.0.1:8080 \
  --state "$HOME/.local/state/pvisor/daemon" \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592 \
  --max-timeout-seconds 86400
```

Replace `/usr/bin/podman` if your trusted installation uses another absolute path. The foreground process reports its listening address after runtime checks. The budgets admit up to 32 sandboxes, four CPU units and 8 GiB of summed hard memory limits; they do not establish an 8 GiB node-wide physical-memory cap. Leave headroom for daemon, helpers and caches, and configure host supervision separately.

In a second shell with the same protected API key, query the lifecycle API:

```bash
curl --fail-with-body --config - <<EOF
url = "http://127.0.0.1:8080/v1/sandboxes"
header = "OPEN-SANDBOX-API-KEY: ${OPEN_SANDBOX_API_KEY}"
EOF
```

Expect a JSON sandbox list, initially empty for fresh state. This verifies API access only, not sandbox creation, native resource enforcement or SDK data-plane readiness. Never publish the key or paste credential-bearing logs into an issue.

## Connect clients {#clients}

Use OpenSandbox SDK 1.1.0 with your chosen domain and protocol, supplying the lifecycle API key. Creation requires `image`, an `entrypoint` argv and `resourceLimits` containing exactly `cpu` and `memory`. An optional `timeout` is in seconds and must be at least 60; omission or null means manual cleanup. No prepared-image name is supplied here because there is no validated drop-in image recipe.

The daemon verifies execd `/ping`, `/ready` and egress `/healthz` before treating creation as ready. A `202` response is a lifecycle response, not a workload exit result. Command/file/metrics traffic goes to the image's real services, rather than to a pVisor imitation of execd.

Bind loopback by default. Before external access, configure TLS at a trusted reverse proxy and set `--public-endpoint HOST:PORT` to the externally routed authority, without scheme or path. Wildcard listeners and port-zero development bindings also require an explicit correct public endpoint. Follow [endpoint authentication](operations.md#endpoints).

## Migration from Cluster {#migration}

The old Cluster task/client SDK, Controller/Worker registration, placement, DAG, lease renewal, completion outbox and artifact-retirement commands are not the daemon interface. Do not reuse old task JSON, `worker.toml`, Controller credentials or journals as daemon input. There is no automatic conversion of distributed task history into sandbox state.

Preserve any old results you need before retiring the old deployment. Start the daemon with a new state directory and migrate callers to the supported OpenSandbox lifecycle profile. Native `pvisor run`, VM execution, local checkpoint/fork and node/cache/memory-pool services remain separate; they are not wired into this daemon backend. See [operations](operations.md) for cleanup and restart behavior.

# Run the single-node daemon

Install `pvisor-daemon` to manage image-based sandboxes on one Linux host through a partial OpenSandbox 1.1.0 API. It owns local admission, lifecycle, durable state and expiration. It does not schedule business tasks across nodes or provide a global DAG, distributed leases, Controller or Workers. Use Kubernetes, Ray or your application for orchestration.

| Need | Guide |
| --- | --- |
| Install and start the API | The commands below |
| Prepare images and choose an execution boundary | [Runtime and integration boundaries](boundaries.md) |
| Inspect, delete and recover sandboxes | [Operations](operations.md) |
| Keep native node/cache/pool services separate | [Service entry points](service.md) |

## Prerequisites {#prerequisites}

Requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and a writable delegated cgroup v2 hierarchy with enabled CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller writes and the KVM API; there is no host, OCI command or registry-pull fallback.

Images are trusted local `images_dir/<key>.json` manifests, not registry references. Fields are absolute independent Linux `rootfs` (never host `/` or overlapping daemon state), absolute guest bootstrap `entrypoint` argv, optional `cmd`, optional `env` and optional absolute firmware `library_dir`. Requested workload argv (or `cmd` if empty) is appended to `entrypoint`; request env overrides manifest env, without host environment inheritance or shell interpolation.

!!! warning
    The bootstrap and image recipe are **not supplied or end-to-end validated**. The old container `cap-drop=ALL` restriction does not describe this native VM backend; an upstream image name is not a native bootstrap/vsock adapter. No fake readiness, SDK-conformance or density evidence is provided. See [image contract](boundaries.md#images).

## Install the executable {#install}

Source installation uses the selected revision and [native build prerequisites](../../community/development.md). These commands are not a validated installation recipe:

```bash
cargo install --locked --path crates/pvisor-daemon --bin pvisor-daemon
pvisor-daemon --help
pvisor-daemon protocol
```

`protocol` prints the pinned OpenSandbox version and commit; it does not certify full SDK compatibility. This source installation is separate from installing the Python `pvisor` package. Do not assume an existing wheel includes the new daemon companion or prepared images. Pin the source revision, SDK 1.1.0 and image contents together.

## Start the API {#start}

`serve` constructs `NativeRuntime` using the implemented, required `--images-dir` and `--cgroup-root` flags. Cargo links `pvisor` and `pvisor-core`; synchronous `main` calls `pvisor::run_krun_internal_if_requested()` before argument parsing or Tokio, then dispatches the hidden `native-supervisor --sandbox-dir ABSOLUTE_PATH` command. The deployment example below uses the current CLI, but does not supply or validate the guest bootstrap, SDK conformance or density.

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592 \
  --max-timeout-seconds 86400
```

Keep runtime paths absolute and unchanged across daemon restarts. Use short state paths such as `/run/user/1000/pvd`: per-sandbox `control.sock` must be shorter than 104 bytes, and vsock Unix sockets also have path limits. Retain state; this `/run` example does not promise persistence across logout/reboot, and VMs cannot survive host reboot. `/sys/fs/cgroup/pvd` must be a real delegated hierarchy, not an ordinary directory.

Choose a private state directory outside the checkout. Generate the API key once and keep the same value in protected service secret storage on restart. The example admits at most 32 records, four CPU units and 8 GiB of summed hard memory limits, not a measured whole-node physical cap; leave daemon/cache/host headroom and configure host supervision separately.

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

An occupied `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

## Migration from Cluster {#migration}

The old Cluster task/client SDK, Controller/Worker registration, placement, DAG, lease renewal, completion outbox and artifact-retirement commands are not the daemon interface. Do not reuse old task JSON, `worker.toml`, Controller credentials or journals as daemon input. There is no automatic conversion of distributed task history into sandbox state.

Preserve old results before retiring the deployment and use new state for the OpenSandbox profile. Native VM execution is wired in `NativeRuntime`; stage/apply and checkpoint/fork APIs are not implemented, and node/cache/pool sharing is not automatically acquired. See [operations](operations.md).

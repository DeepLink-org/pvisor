# Run the single-node daemon

Install `pvisor-daemon` to manage image-based sandboxes on one Linux host through a partial OpenSandbox 1.1.0 API. It owns local admission, lifecycle, durable state and expiration. Use Kubernetes, Ray or your application for cross-node task scheduling, global DAGs and distributed leases; the daemon includes no Controller or Workers.


## Prerequisites {#prerequisites}

Requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and a writable delegated cgroup v2 hierarchy with enabled CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller writes and the KVM API; there is no host, OCI command or registry-pull fallback.

Images are trusted local `images_dir/<key>.json` manifests, not registry references. Fields are absolute independent Linux `rootfs` (never host `/` or overlapping daemon state), absolute guest bootstrap `entrypoint` argv, optional `cmd`, optional `env` and optional absolute firmware `library_dir`. Requested workload argv (or `cmd` if empty) is appended to `entrypoint`; request env overrides manifest env, without host environment inheritance or shell interpolation.

!!! warning
    Prepare your own guest bootstrap and vsock service bridges following the [image contract](boundaries.md#images). The project **has not supplied a bootstrap or image recipe or performed end-to-end validation**; SDK-conformance validation and density measurements are also unavailable.

## Install the executable {#install}

Prepare the [native build prerequisites](../../community/development.md) and run these source-installation commands at your selected revision. This installation path has not yet been validated:

```bash
cargo install --locked --path crates/pvisor-daemon --bin pvisor-daemon
pvisor-daemon --help
pvisor-daemon protocol
```

`protocol` prints the pinned OpenSandbox version and commit. Source installation is separate from installing the Python `pvisor` package; when using a wheel, check whether that version includes the daemon and prepare images separately. Pin the source revision, SDK 1.1.0 and image contents together; SDK conformance still needs end-to-end validation.

## Start the API {#start}

`serve` constructs `NativeRuntime` using the implemented, required `--images-dir` and `--cgroup-root` flags. Cargo links `pvisor` and `pvisor-core`; synchronous `main` calls `pvisor::run_krun_internal_if_requested()` before argument parsing or Tokio, then dispatches the hidden `native-supervisor --sandbox-dir ABSOLUTE_PATH` command. Once the images and cgroup hierarchy are ready, start the API with the following CLI configuration.

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

Choose a private state directory outside the checkout. Generate the API key once and keep the same value in protected service secret storage on restart. The example sets an admission budget of 32 records, four CPU units and 8 GiB of summed hard memory limits. Whole-node physical usage has not been measured; leave daemon/cache/host headroom and configure host supervision separately.

In a second shell with the same protected API key, query the lifecycle API:

```bash
curl --fail-with-body --config - <<EOF
url = "http://127.0.0.1:8080/v1/sandboxes"
header = "OPEN-SANDBOX-API-KEY: ${OPEN_SANDBOX_API_KEY}"
EOF
```

Expect a JSON sandbox list, initially empty for fresh state. This query verifies API access; sandbox creation, native resource enforcement and SDK data-plane readiness need separate validation. Never publish the key or paste credential-bearing logs into an issue.

## Enable the daemon memory pool {#memory-pool}

Append `--memory-pool` to the `serve` command above to connect new sandboxes to a daemon-owned physical shared-page pool. It is off by default. The runtime scans resident 4 KiB pages, admitting cross-instance duplicate candidates; the daemon holds one physical copy. Reads retain sharing, writes create private pages through kernel COW, and scanning then releases obsolete references. The runtime parks CPUs, drains device access and rechecks live contents before replacing mappings. This Linux path does not require userfaultfd and does not compress unique pages. Actual usage depends on duplication, scanning and writes; see the [memory benchmark](../../benchmarks/vm-memory/index.md#linux-lifecycle).

The shared pool and idle release work together without another switch. When the guest reports unused pages, the runtime replaces their mappings and releases pool references before acknowledging that the guest can reuse the memory, avoiding retention of unused contents.

The component runs independently of the API process and retains pages across API restart. After disconnect, pidfd-confirmed VM process exit is required before releasing mapped references, preventing reuse of slots still being read. Pool loss fails dependent VMs; live pools are not automatically replaced. Default limits are 512 MiB of page data, 32,768 objects, 32 connections and 32,768 references per connection: at 4 KiB per object, the object ceiling limits the default pool to 128 MiB of distinct pages and each VM to 128 MiB of references. Unique candidates retain bounded hashes only; budget rejection keeps the original page. Indexes, mappings, threads and allocator memory add overhead. The pool is outside individual sandbox cgroups and admission limits; reserve host headroom. Scanning skips nonresident pages; budget for startup and write peaks. Drain old VMs and use fresh state when upgrading from the compressed pool. VM/pool tests do not establish complete OpenSandbox SDK end-to-end conformance.

## Connect clients {#clients}

Use OpenSandbox SDK 1.1.0 with your chosen domain and protocol, supplying the lifecycle API key. Creation requires `image`, an `entrypoint` argv and `resourceLimits` containing exactly `cpu` and `memory`. An optional `timeout` is in seconds and must be at least 60; omission or null means manual cleanup. No prepared-image name is supplied here because there is no validated drop-in image recipe.

The daemon verifies execd `/ping`, `/ready` and egress `/healthz` before treating creation as ready. A `202` response means the lifecycle request was accepted; obtain workload exit results through the command interface. Command/file/metrics traffic is forwarded to the image's execd and egress services.

Bind loopback by default. Before external access, configure TLS at a trusted reverse proxy and set `--public-endpoint HOST:PORT` to the externally routed authority, without scheme or path. Wildcard listeners and port-zero development bindings also require an explicit correct public endpoint. Follow [endpoint authentication](operations.md#endpoints).

An occupied version-1 `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

Current v2 state stores a small `sandboxes.json` owner/header with an empty map plus private per-sandbox `records/`. A missing native `owner.json` still rejects v2 startup; the empty header is not proof that no sandbox exists. Preserve all ownership/record state together. Native v1 storage migrates only after the runtime factory accepts the owner; see [storage migration](../../design/daemon/storage.md#migration).

## Migration from Cluster {#migration}

The old Cluster task/client SDK, Controller/Worker registration, placement, DAG, lease renewal, completion outbox and artifact-retirement commands are not the daemon interface. Do not reuse old task JSON, `worker.toml`, Controller credentials or journals as daemon input. There is no automatic conversion of distributed task history into sandbox state.

Preserve old results before retiring the deployment and use new state for the OpenSandbox profile. Native VM execution is wired in `NativeRuntime`; stage/apply and checkpoint/fork APIs are not implemented, and node/cache/pool sharing is not automatically acquired. See [operations](operations.md).

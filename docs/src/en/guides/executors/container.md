# Native OCI containers

The container executor runs OCI userspace on a Linux host kernel, invoking runc or crun directly without Docker/Podman.

```bash
pvisor run --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

Container image selects the executor automatically; explicit executor makes this visible. `--container-rootfs PATH` supplies a prepared rootfs instead of image preparation through pVisor's OCI store.

## Execution path

pVisor creates an OCI bundle, mounts a matching static Linux amd64/arm64 pVisor binary, and invokes host execution with `--spec` and a RunSpec inside the container. Agent arguments live in the spec rather than the OCI runner argv. Inner pVisor owns its AgentCtl and returns typed results.

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

## Network modes

| Mode | Purpose | Limit |
| --- | --- | --- |
| host | Gateway or explicit OverlayNet proxy | Required for host-loopback proxy addresses |
| none | Offline | Incompatible with proxy/Gateway needing host network |
| bridge | — | Requires external CNI; currently rejected |

## Gaps

- Container isolation is recorded without claiming complete capability enforcement.
- `--safe` mode therefore rejects startup instead of offering an incomplete boundary.
- Selective host-network policies remain cooperative.

Use a [VM](vm.md) for an independent kernel or mandatory selective networking.

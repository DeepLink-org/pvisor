# Native OCI containers

The container executor runs the userspace of an OCI image on the Linux host kernel. It calls `runc` or `crun` directly and does not need Docker or Podman.

```bash
pvisor spawn --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

`--container-image IMAGE` selects the container executor automatically; `--executor container` makes the choice explicit. `--container-rootfs PATH` uses an existing rootfs directly; otherwise pVisor prepares the image with its bundled OCI image store.

`--container-platform linux/amd64` or `linux/arm64` is an optional native architecture assertion, not a cross-architecture execution or download selector. A matching assertion is accepted; a mismatched assertion is rejected before launch, including with `--container-rootfs`. Host and VM configurations reject this option rather than ignoring it. In TOML, use `container.platform = "linux-amd64"` or `"linux-arm64"`.

Install `runc` (the default) or select `crun` explicitly. The example below assumes Linux x86_64. See the [CLI reference](../../reference/cli.md), [configuration fields](../../reference/config.md#settings) and [container cases](../../reference/cases.md).

## How it works

pVisor prepares the workspace and proxy on the host and writes the workload argv into the OCI bundle. The OCI runtime starts the workload. One host Session owns the deadline, output, result, and any workspace changes retained for review.

```bash
pvisor spawn \
  --container-image example/codex-agent:latest \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

## Network modes

| Mode | Purpose | Limit |
| --- | --- | --- |
| `--container-network host` | Use a Gateway or an explicit OverlayNet proxy | Required: the injected proxy address is host loopback |
| `--container-network none` | Run offline | Cannot be combined with a proxy or Gateway that needs host networking |
| `bridge` | — | Requires external CNI configuration; currently rejected |

## Known gaps

- The container executor records container isolation; **complete capability enforcement evidence is unavailable**.
- `--safe` refuses to start on a container.
- With host networking, selective network policy is cooperative, just as it is on the host.

Use a [VM](vm.md) when you need an independent kernel or a non-bypassable network boundary.

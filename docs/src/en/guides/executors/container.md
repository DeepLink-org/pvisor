# Native OCI containers

The container executor runs the userspace of an OCI image on the Linux host kernel. It calls `runc` or `crun` directly and does not need Docker or Podman.

```bash
pvisor run --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

`--container-image IMAGE` selects the container executor automatically; `--executor container` makes the choice explicit. `--container-rootfs PATH` uses an existing rootfs directly; otherwise pVisor prepares the image with its bundled OCI image store.

`--container-platform linux/amd64` or `linux/arm64` is an optional native architecture assertion, not a cross-architecture execution or download selector. A matching assertion is accepted; a mismatched assertion is rejected before launch, including with `--container-rootfs`. Host and VM configurations reject this option rather than ignoring it. In TOML, use `container.platform = "linux-amd64"` or `"linux-arm64"`.

The injected executable defaults to the running pVisor. Supply `--container-pvisor-binary PATH` only for a compatible Linux build when needed; pVisor does not discover or download another executable from the platform assertion. You must provision a rootfs and binary compatible with the native architecture and guest ABI. The example below assumes Linux x86_64. See the [CLI reference](../../reference/cli.md), [configuration fields](../../reference/config.md#settings) and [container cases](../../reference/cases.md).

## How it works

pVisor generates a standard OCI bundle, mounts the current or explicitly supplied compatible Linux pVisor binary into the rootfs, and then takes the ordinary `pvisor run --executor host --spec ...` path inside the container. The agent command lives in the RunSpec and is not exposed in the OCI runner's argv; the pVisor inside the container creates its own AgentCtl and returns typed results.

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
| `--container-network host` | Use a Gateway or an explicit OverlayNet proxy | Required: the injected proxy address is host loopback |
| `--container-network none` | Run offline | Cannot be combined with a proxy or Gateway that needs host networking |
| `bridge` | — | Requires external CNI configuration; currently rejected |

## Known gaps

- The container executor records container isolation but **does not claim complete capability enforcement**.
- `--safe` therefore refuses to start on a container rather than offering an incomplete boundary.
- With host networking, selective network policy is cooperative, just as it is on the host.

Use a [VM](vm.md) when you need an independent kernel or a non-bypassable network boundary.

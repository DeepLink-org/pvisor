# Choose an execution environment

Choose the executor for the kernel and userland your command needs. Ordinary host Runs write through to the workspace; `--safe` stages writes. Pass `--stage PATH` to retain them for review.

| Executor | Environment | Requirements |
| --- | --- | --- |
| `host` (default) | Host kernel and installed tools | Staged runs need the platform's filesystem mount support; macOS uses macFUSE |
| `container` | OCI image userland on a Linux host kernel | Native OCI runtime (`crun` or `runc`) and a matching Linux pVisor binary |
| `vm` | Linux guest kernel supplied by libkrun | Linux KVM or Apple Silicon HVF; a Linux rootfs for the host architecture |

The command must exist in the selected environment. An Ubuntu image does not include your Agent just because it is installed on the host. Consult the Run Bundle for installed controls, warnings, and platform limitations.

## Host command

```bash
pvisor run --executor host --stage ../stage-host -- /bin/sh
```

The working directory keeps its original path inside the sandbox. Ordinary Runs write through to the lower workspace; `--safe` uses a copy-on-write view that pVisor discards at Run exit unless `--stage` is specified. Explicit writable mounts and application state outside the workspace can still persist immediately. Safe-best-effort isolation reports unsupported controls; it is not a uniform guarantee across platforms.

## Linux container

```bash
pvisor run --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

This uses the native OCI executor. Additional mounts and the injected pVisor binary are configured through the [CLI reference](../reference/cli.md). Gateway and the explicit OverlayNet proxy currently require container host networking because their addresses are host loopback endpoints.

## VM with an OCI image

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 \
  --stage ../stage-vm --mount "$PWD:stage" -- /bin/sh
```

`--rootfs DIR` uses a prepared Linux rootfs instead. Image resolution is daemonless; the VM executor does not need Docker. Pin an image digest when reproducibility matters. Rootfs writes are temporary; the workspace stage persists for review.

On Linux, `--rootfs host` can expose the host root as a lower layer to a separate guest kernel. This exposes host contents for reading. Declare any additional workspace share with `--mount`; use this layout only for same-owner local work.

Linux needs accessible `/dev/kvm`. On macOS, `just build release` builds and signs the binary with the Hypervisor entitlement; source builds also need Zig. VM networking supports policy-controlled IPv4 TCP, DHCP and synthetic DNS; UDP application traffic, IPv6, QUIC and inbound connections are outside the current network surface.

## Filesystem access and decisions

`--mount SOURCE[:TARGET]:read|stage|write` exposes a host path; an omitted target equals the source. `read` and `stage` add lower layers to the workspace view, and `write` grants direct persistent host writes. `--access PATH-GLOB:deny|read` applies a rule within the overlay view: `deny` hides matching paths, while `read` currently warns on access. The current implementation does not enforce `read` as an immutable per-path permission. Keep `--stage` outside every lower layer.

With `--stage PATH`, changes remain for an explicit `review`, `apply`, or `drop` decision. With `--safe` alone, the temporary stage is removed at Run exit. Without either option, ordinary host Runs write through. A host process Run cleans up its process group on completion or timeout and bounds output draining; detached descendants outside that group are not covered by process-group cleanup alone.

Continue with [Review and apply](review-apply.md) or [Network policy](network.md).

# Choose an execution environment

Choose the executor for the kernel and userland your command needs. Enable a stage separately when you want to review workspace writes.

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

Host execution preserves the host filesystem view by default. Use `--filesystem sandbox` for path access restrictions, `--stage PATH` for a reviewable copy-on-write workspace, and `--overlaynet-deny-all` for deny-all network policy. These settings are independent; a network option does not enable filesystem restrictions.

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
  --overlayfs-path /workspace --stage ../stage-vm -- /bin/sh
```

`--rootfs DIR` uses a prepared Linux rootfs instead. Image resolution is daemonless; the VM executor does not need Docker. Pin an image digest when reproducibility matters. Rootfs writes are temporary; the workspace stage persists for review.

On Linux, `--rootfs host` can expose the host root as a read-only lower layer to a separate guest kernel. This exposes host contents for reading. Keep an explicit workspace `--overlayfs-path`; use this layout only for same-owner local work.

Linux needs accessible `/dev/kvm`. On macOS, `just build release` builds and signs the binary with the Hypervisor entitlement; source builds also need Zig. VM networking supports policy-controlled IPv4 TCP, DHCP and synthetic DNS; UDP application traffic, IPv6, QUIC and inbound connections are outside the current network surface.

## Compose and commit

`--overlayfs-compose DIR` adds read-only layers in bottom-to-top order above the current workspace. `--overlayfs-path PATH` sets the absolute command-visible workspace path. Keep the stage outside all lower layers.

| Commit mode | Behavior |
| --- | --- |
| `manual` (default) | Retain changes for `review`, `apply`, or `drop` |
| `apply` | Automatically apply at Run exit, including a nonzero command exit |
| `drop` | Discard the stage at Run exit |

Use manual mode when accepting changes depends on tests or review. A host process Run cleans up its process group on completion or timeout and bounds output draining; detached descendants outside that group are not covered by process-group cleanup alone.

Continue with [Review and apply](review-apply.md) or [Network policy](network.md).

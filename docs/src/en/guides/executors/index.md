# Choose an executor

Choose an executor by the kernel and userspace your command needs. Stage workspace changes with `--safe` or an explicit `--stage PATH` and review them afterward; for the default write and cleanup rules see [Staging and storage](../../reference/cli.md#暂存与存储).

| Executor | Environment | Prerequisites | Guide |
| --- | --- | --- | --- |
| `host` (default) | Host kernel and installed tools | Platform filesystem-mount support for staged runs; macFUSE on macOS | [host](host.md) |
| `container` | OCI image userspace on the Linux host kernel | A native OCI runtime (`crun` or `runc`) and a matching Linux pVisor binary | [container](container.md) |
| `vm` | Linux guest kernel provided by libkrun | Linux KVM or Apple Silicon HVF, plus a Linux rootfs matching the host architecture | [VM](vm.md) |

The command must be installed in the chosen environment: a host-installed agent is not present in an Ubuntu image. The controls actually installed, the warnings, and the platform limits are defined by the Run Bundle; for each executor's boundary on every capability dimension see [Executor boundaries](../../security/executor-boundaries.md).

## Choosing

- **Just want to review file changes with local tooling**: host with `--safe`.
- **A mandatory network boundary**: VM (`--overlaynet auto`), or host with `--overlaynet-deny-all`.
- **A fixed Linux userspace**: container or VM with an OCI image.
- **An independent kernel**: VM.

`--safe` does not choose an executor for you; it requires the chosen executor to enforce file and network isolation, and it refuses to start instead of silently degrading.

## Options shared by every executor

`--mount SOURCE[:TARGET]:stage` adds a host directory as the workspace view's lower layer. `--mount SOURCE:read` grants read-only access at the original absolute path and requires the host executor with `--safe/--ask`; `SOURCE:write` grants direct write access to the host path. read/write cannot remap TARGET and do not go through the workspace's approval rules. See [Filesystem parameters](../../reference/cli.md#文件系统参数) for overlap limits.

`--access PATH-GLOB:deny|ask|warn` denies, asks, or allows with a warning in the OverlayFS view; see [File policies](../policies/files.md). `--stage PATH` selects the staging location.

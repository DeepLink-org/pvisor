# Choose an executor

Choose by kernel and userspace requirements. Enable staging with `--safe` or an explicit `--stage PATH` and review afterward. Defaults are defined in [storage](../../reference/cli.md#暂存与存储).

| Executor | Environment | Prerequisites | Guide |
| --- | --- | --- | --- |
| host (default) | Host kernel and installed tools | Platform mounting support for staging; macFUSE on macOS | [Host](host.md) |
| container | OCI userspace on Linux host kernel | Native crun/runc and matching Linux pVisor binary | [Container](container.md) |
| vm | Linux guest kernel via libkrun | Linux KVM or Apple Silicon HVF; matching Linux rootfs | [VM](vm.md) |

Install the command inside the selected environment. A host-installed agent is not present automatically in an Ubuntu image. Inspect Bundle controls, warnings, and platform limits; see [boundaries](../../security/executor-boundaries.md).

## Choosing

- Local tools and file review: host with `--safe` mode.
- Mandatory networking: VM `--overlaynet auto` or host `--overlaynet-deny-all`.
- Fixed Linux userspace: container or VM with an image.
- Independent kernel: VM.

`--safe` does not select an executor. It requires the chosen executor to supply file/network isolation or rejects startup.

## Shared options

`--mount SOURCE[:TARGET]:stage` adds a host directory as a workspace lower layer. `SOURCE:read` grants read-only original-path access, requiring host plus `--safe`/`--ask`; `SOURCE:write` writes directly to host. Read/write cannot remap TARGET or use workspace approval rules. See [filesystem parameters](../../reference/cli.md#文件系统参数).

`--access PATH-GLOB:deny|ask|warn` controls the OverlayFS view; see [file policy](../policies/files.md). `--stage PATH` selects storage.

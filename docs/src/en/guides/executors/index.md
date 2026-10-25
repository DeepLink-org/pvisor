# Choose an executor

The two quick-start examples use Host and VM respectively. When you need a different execution environment, choose by the kernel and userspace your command requires.

| Executor | Environment | Prerequisites | Guide |
| --- | --- | --- | --- |
| `host` (default) | Host kernel and installed tools | Platform filesystem-mount support for staged runs; macFUSE on macOS | [host](host.md) |
| `container` | OCI image userspace on the Linux host kernel | A native OCI runtime (`crun` or `runc`) and a matching Linux pVisor binary | [container](container.md) |
| `vm` | Linux guest kernel provided by libkrun | Linux KVM or Apple Silicon HVF, plus a Linux rootfs matching the host architecture | [VM](vm.md) |

The command must be installed in the chosen environment; for a container or VM, install the required agent in its image.

## Choosing

- **Just want to review file changes with local tooling**: host with `--safe`.
- **A mandatory network boundary**: VM (`--overlaynet auto`), or host with `--overlaynet-deny-all`.
- **A fixed Linux userspace**: container or VM with an OCI image.
- **An independent kernel**: VM.

Select an executor with `--executor`. `--safe` requires file and network isolation and refuses to start if it cannot enforce them. The container executor does not support `--safe`; see the [container executor guide](container.md) for staging and network configuration.

<a id="options-shared-by-every-executor"></a>

## Configuration and verification {#configuration-and-verification}

After choosing an executor, follow its guide in the table to prepare the environment. See [CLI filesystem parameters](../../reference/cli.md#文件系统参数) for mounts, path access, and staging; configure [file policies](../policies/files.md) to restrict specific files.

After execution, use `pvisor status --review` to check the controls actually installed and warnings in the Run Bundle. See [Executor boundaries](../../security/executor-boundaries.md) for isolation scope and platform limits.

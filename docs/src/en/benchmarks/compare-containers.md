# Comparison: Docker / devcontainer

Docker plus Git is a capable development workflow. pVisor adds pending changes, preimage checks before apply, selective admission and records of observed controls. If a reliable worktree/patch review pipeline already exists, the value is whether those steps should become one protocol.

## Scope

Documentation checked on 2026-10-04. The local Docker daemon was inaccessible; measured containers use rootless Podman + crun. This is an OCI control, not a Docker measurement. [Filesystem results](filesystem.md) retain image/rootfs hashes and parameters; [VM startup](startup.md) retains macOS and Linux history.

| Configuration | Where edits happen | Admission protection | Suitable use |
|---|---|---|---|
| Docker + writable bind | Host files change immediately | Review and rollback require an additional workflow | Trusted tasks needing reproducible dependencies |
| Docker writable layer/volume | Container layer or volume | Export files, patches or commits and merge separately | Independent or remote workspaces |
| devcontainer | Configured mounts/volumes and tools | Combine with Git, worktrees and PRs | Standard team development environments |
| Docker + independent worktree + Git | Independent files; original workspace can remain intact | Patch checks and three-way merge; caller owns retries and workspace management | An established Git review pipeline |
| pVisor staged Job | Upper stage pending until apply | Preimage conflicts, selective apply/drop and transaction recovery | Shared review across agents or non-Git directories |

Sources: Docker [bind mounts](https://docs.docker.com/engine/storage/bind-mounts/), [security](https://docs.docker.com/engine/security/), and the [devcontainer specification](https://containers.dev/). Git workflows must also handle untracked/binary files, permissions, symlinks and concurrent edits.

## Costs and workflow differences

[Filesystem measurements](filesystem.md) compare identical tools and inputs; image preparation is excluded. [Apply/drop](apply.md) measures scale and conflict rejection. Check the Bundle's staging scope: a container mount is not automatically staged.

Docker plus a complete Git admission workflow can be sufficient. `git diff` alone cannot retract changes already made through a writable bind mount or produce pVisor's observed-capability record. pVisor's process, staging and recording costs buy that shared workflow.

## Corrections

Submit configuration, image digest and reproduction to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues). Docker/overlay2 measurements are welcome; Podman results do not establish Docker Desktop performance.

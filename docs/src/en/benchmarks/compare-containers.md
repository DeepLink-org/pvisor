# Comparison: Docker / devcontainer

Docker plus Git is a capable development workflow. pVisor adds pending changes, preimage checks before apply, selective admission and records of observed controls. If a reliable worktree/patch review pipeline already exists, the value is whether those steps should become one protocol.

## Scope

The 2026-10-04 follow-up measures a private, user-owned rootless Docker Engine 29.7.2 on the same Linux host, despite an inaccessible system daemon. It uses the same Python/Node/Rust/Claude/Codex artifacts, projects and two-core budget with prepared images. Earlier Podman/crun data remain separate. Docker Desktop, devcontainer extensions and remote environments are unmeasured.

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

### What these numbers support {#reference-comparison}

Complete repair/testing P50 is **0.90 s** in Docker, **0.70 s** in pVisor staged, and **3.97 s** in pVisor VM. Docker metadata/read/write remain close to native. Staging adds about 16 ms for 64 MiB reads and 175 ms to traverse 2,048 files. Lightweight staging costs hundreds of milliseconds here; an independent guest kernel currently brings seconds of additional tool-path cost.

This is not a ranking within identical security boundaries. Staged host supplies pending changes/evidence while retaining access outside the workspace. Docker supplies namespaces with a writable bind mount; the VM supplies a guest kernel and staged view. Select using the required boundary, task time and admission workflow together.

Real clients add costs: Claude takes **1.23 s** in Docker and **1.07 s** staged; Codex takes **6.26 s** in Docker, **2.25 s** staged, and **10.93 s** in the VM. Claude/VM initialization times out and has no successful latency sample. These are fixed tools and controlled responses. Real inference, default client inner sandboxes and Docker writable-layer/overlay2 file workloads remain unmeasured.

[Complete environment/distributions](agent-tasks.md#reference-env) · [File operations](filesystem.md#reference-fs) · [Memory scope/configuration](methodology.md#reference-env)


## Corrections

Submit configuration, image digest and reproduction to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues). Docker writable-layer/overlay2 and Docker Desktop samples are welcome; this measures only Linux rootless Engine with bind mounts.

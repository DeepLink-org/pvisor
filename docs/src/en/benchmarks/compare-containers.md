# Do you need pVisor alongside Docker or devcontainers?

## Main conclusions {#conclusions}

**Keep a Docker + worktree/Git workflow when it meets your needs. pVisor staged adds retained changes and selective application, with extra file-access cost. The measured Docker VFS creation cost does not represent overlay2 or Docker Desktop.**

| Need | Selection implication |
|---|---|
| Existing Docker + worktree/Git | Keep the workflow and compare tool costs |
| Unified staging and selective application across Agents | Evaluate pVisor host staged |
| Independent guest kernel | Compare VM execution costs |

## Motivation {#motivation}

Containers supply tool environments, while mounts decide whether changes immediately reach the host. Review, export, conflict handling and application also contribute to final workflow cost.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

Both use fresh validated fixtures. Operation timers exclude launch/exit; complete tasks include both. devcontainer plugins, overlay2, Docker Desktop and complete container-plus-Git review workflows are unmeasured. See [review and selective application](supervision-cost.md) for the complete local stage versus Git worktree and reflink comparison.

## Data and analysis {#results}

### Local tasks and files {#reference-comparison}

Startup/filesystem: 2026-10-05; repair: 2026-10-06. Independent workload cells N=60, failures=0, three warmups. P50 or cluster medians with counts; do not pool across workloads.

| Operation | Unit | Native | pVisor staged | Docker rootless / VFS | pVisor VM |
|---|---|---|---|---|---|
| First output | ms | 1.24 | 24.99 | 3380.93 | 99.76 |
| Repair through exit | s | 0.45 | 0.68 | 5.15 | 3.25 |
| Seven tools through exit | s | 0.51 | 1.29 | 5.68 | 6.66 |
| Traverse 2,048 files | ms | 4.68 | 74.90 | 5.03 | 227.11 |
| Read/verify 64 MiB | ms | 33.28 | 68.85 | 32.48 | 154.80 |
| Offline npm install | ms | 190.34 | 263.19 | 273.68 | 1662.23 |

Bind-mount tool access can be fast while VFS creation is slow. Reused containers amortize creation; disposable environments still pay it. Staged originals remain unchanged until apply.

### Change workflow

| Configuration | Change/review workflow |
|---|---|
| Docker writable bind | Host files change directly; independent worktree can be added |
| Docker layer / volume | Export, patch or commit for application |
| devcontainer | Configured mounts/volumes plus Git/PR review |
| pVisor staged | Retained changes, path selection, preimage conflict checks |

Official [Docker bind-mount docs](https://docs.docker.com/engine/storage/bind-mounts/) explain default host writes; [devcontainer specification](https://containers.dev/) explains configuration. Compare [isolation](isolation-tests.md) and [apply](apply.md) as well as tool speed. Version-pinned [Agent CLI tests](agent-tasks.md#cli-compatibility) use controlled responses, without a default built-in-sandbox ranking.

### Downloads and reproduction {#run}

[Derived table CSV](compare-containers.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

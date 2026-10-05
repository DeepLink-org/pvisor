# Comparison: Docker / devcontainer

## Main conclusions {#conclusions}

**Docker bind-mount file access is near native and ahead of pVisor's staged path; measured short staged repair tasks are close to Docker and slightly faster.** Same-tool repair/tests take **0.70 s** staged, **0.90 s** Docker and **3.97 s** pVisor VM. pVisor's value is retained changes, conflict checks and selective application; “faster than Docker across the board” would be inaccurate.

Established Docker + worktree/Git review pipelines remain useful. Consider staged when multiple Agents or non-Git directories need one application protocol.

## Motivation {#motivation}

Containers supply tool environments, while mounts decide whether changes immediately reach the host. Review, export, conflict handling and application also contribute to final workflow cost.

## Experiment design {#interpretation}

Linux same-host comparisons share two cores, Python/Node/Rust/Agent tools and inputs. Docker Engine 29.7.2 is rootless, with prepared images, running daemon and writable bind mount. Cells have three warmups and 30 samples. Complete tasks include launch-to-verified-result; file operations exclude startup. Results use pinned pVisor artifacts and have not all been rerun with current integrated filesystem artifacts. Docker Desktop, devcontainer plugins and overlay2 workloads are unmeasured.

| Configuration | Change location and review workflow |
|---|---|
| Docker writable bind mount | Mounted host files change directly; independent worktrees can be added |
| Docker writable layer / volume | Changes stay in the layer or volume; export, patches or commits handle application |
| devcontainer | Configured mounts or volumes, with Git/PR review workflows |
| pVisor staged | Stage retains changes before apply; selective paths and preimage conflict checks |

See Docker's [bind-mount documentation](https://docs.docker.com/engine/storage/bind-mounts/) for default host writes and the [devcontainer specification](https://containers.dev/) for configuration.

## Data and analysis {#results}

### Same-tool tasks {#reference-comparison}

| Operation | pVisor staged P50 | Docker P50 | pVisor VM P50 |
|---|---:|---:|---:|
| Repair and tests | 0.70 s | 0.90 s | 3.97 s |
| Controlled Claude tool loop | 1.07 s | 1.23 s | Initialization timeout / N=0 |
| Controlled Codex tool loop | 2.25 s | 6.26 s | 10.93 s |
| Read/verify 64 MiB | 48.77 ms | 33.24 ms | 89.27 ms |
| Traverse 2,048 files | 180.13 ms | 5.06 ms | 310.54 ms |

Staged short repair is about 0.20 s quicker, while Docker individual file operations remain near native and staged metadata costs more. Client initialization and tool combinations affect overall results; individual file speed does not replace complete-task measurements. CLIs use controlled responses without inference, rather than comparing default built-in sandboxes.

pVisor VM has an independent guest kernel; staged host, Docker namespaces and VMs have different boundaries. Consider [isolation](isolation-tests.md) and [apply costs](apply.md). [Filesystem performance](filesystem.md) provides current measurements; these are not combined with this table's Docker timings into precise ratios.

[Complete tasks and distributions](agent-tasks.md#reference-env) · [Same-tool file data](filesystem.md#reference-fs) · [Protocol and artifacts](methodology.md#reference-env)

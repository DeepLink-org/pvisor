# Which execution modes block host access outside the workspace?

## Main conclusions {#conclusions}

**In the tested Linux configurations, pVisor staged, safe and VM allow inside-view operations, block the listed outside host accesses and retain workspace writes in stage across 3/3 checks each. pVisor OCI and Podman also block outside access, but their authorized writable workspace mounts modify host files directly. Host allows the listed host accesses.**

| Need | Selection implication |
|---|---|
| Review execution results and merge selected workspace changes | Choose a validated staged, safe or VM configuration |
| Use an OCI writable workspace mount | Writes reach the host directly; provide review and rollback separately |
| Allow access to host paths | Tested host keeps host paths visible and is not a workspace file sandbox |

## Motivation {#motivation}

Choosing an execution mode requires knowing both the accessible paths and where writes land. A successful command alone cannot distinguish a staged write from a host modification. The boundary also determines which performance controls offer comparable semantics.

## Experiment design {#interpretation}

Seven configurations each run three fresh fixtures. A fixed seed shuffles configurations in each repetition, giving 21 attempts. Native and host provide host-accessible controls. Podman uses private overlay storage and a writable workspace mount; pVisor OCI uses crun and an explicit writable mount. VM and OCI use the same prepared Python/Git rootfs.

Each fixture must read the inside-view file, write and read back a workspace file, and use an internal Unix socketpair. Outside probes cover absolute paths, symlinks, `/proc/self/root`, parent traversal, absolute and symlink writes, a host Unix socket connection and an absolute lower-workspace alias write. Validation checks actual output, final host bytes, staged content and the Bundle's execution boundary rather than syscall status alone.

The environment is Linux/x86_64 on an AMD Ryzen 7 9700X with kernel 7.2.8-200.fc44. Each payload verifies CPU affinity: CPUs 0 and 1 on the host, two guest vCPUs in VM. VM memory is configured to 1 GiB; there is no uniform host memory cap. There are no warmups or host cache eviction. These correctness repetitions do not produce a latency ranking. Prepared inputs are verified before and after execution, and an independent publication audit rechecks all retained evidence. Failures are counted separately and never filled with older samples.

## Data and analysis {#results}

Measured on 2026-10-06. N=3 per mode, 21/21 conditions passing with no failures. Access/state columns report occurrences out of three, not performance statistics. Each of the four read paths is checked separately.

| Configuration | Checks passed | Inside read/write/socket | Four outside read paths | Outside host written | Outside Unix socket reachable | Host lower alias written | Workspace writes staged |
|---|---:|---|---|---:|---:|---:|---:|
| Native | 3/3 | 3/3 each | 3/3 each | 3/3 | 3/3 | 3/3 | 0/3 |
| pVisor host | 3/3 | 3/3 each | 3/3 each | 3/3 | 3/3 | 3/3 | 0/3 |
| pVisor staged | 3/3 | 3/3 each | 0/3 each | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor safe | 3/3 | 3/3 each | 0/3 each | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor vm | 3/3 | 3/3 each | 0/3 each | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor OCI (writable mount) | 3/3 | 3/3 each | 0/3 each | 0/3 | 0/3 | 3/3 | 0/3 |
| Podman (writable mount) | 3/3 | 3/3 each | 0/3 each | 0/3 | 0/3 | 0/3 | 0/3 |

Every mode passes the inside read, write and socket positives, preventing an unusable environment from being mistaken for an effective boundary. Staged, safe and VM leave host lower unchanged and retain the complete staged write. Safe and VM return success for the lower-alias write API while still leaving the host unchanged. pVisor OCI writes directly to lower through the tested alias. Podman does not resolve that absolute host alias, but its relative workspace write also reaches the host directly.

These are path and Unix socket fixture results. TCP policy is covered separately in [network](network.md), and merge conflicts/interruption recovery in [apply](apply.md).

### Scope {#acceptance}

Mount grants, rootfs and configuration determine the boundary; other configurations require separate validation. These checks are not a kernel-vulnerability or comprehensive escape audit and do not establish rollback of remote side effects. macOS remains unmeasured. See [executor boundaries](../security/executor-boundaries.md) for execution modes and current defaults.

### Downloads and reproduction {#run}

[Derived matrix CSV](isolation-tests.csv) · [Artifact and audit provenance CSV](isolation-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

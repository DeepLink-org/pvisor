# Do executors block writes outside the workspace?

## Motivation {#motivation}

Choose an execution mode by both the accessible paths and where writes land. A successful command alone cannot distinguish a staged write from a host modification, and the boundary also determines which performance comparisons share the same semantics.

In the tested Linux configurations, pVisor staged, safe and VM allowed inside-view operations, blocked the listed outside host accesses and kept workspace writes in stage, 3/3 checks each. pVisor OCI and Podman also blocked outside access, but their authorized writable workspace mounts modify host files directly. Host allowed the listed host accesses.

<!-- measurement-status:start -->

These numbers and conclusions remain the original historical cohorts; the new framework has no matching rerun. See [measurement coverage](index.md#measurement-coverage).

<!-- measurement-status:end -->

## Experiment design {#interpretation}

Check each execution mode against a fixture with known paths — what is allowed, what is denied, where writes land — as the isolation boundary it actually enforces. The tighter that boundary, the fewer host paths stay reachable or modifiable.

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

These are path and Unix socket fixture results. TCP policy is covered separately in [network](network.md), and merge conflicts/interruption recovery in [apply](supervision-cost.md#apply-cost).

### Scope {#acceptance}

Mount grants, rootfs and configuration determine the boundary; other configurations require separate validation. These checks are not a kernel-vulnerability or comprehensive escape audit and do not establish rollback of remote side effects. macOS remains unmeasured. See [executor boundaries](../security/executor-boundaries.md) for execution modes and current defaults.

### Sources and reproduction {#run}

Complete CSV/JSON and raw records remain in their local cohorts or the documentation data archive.

[Comparison method](index.md#interpretation) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

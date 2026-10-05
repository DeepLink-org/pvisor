# Which execution modes block host access outside the workspace?

## Main conclusions {#conclusions}

**Choose the isolation boundary before comparing speed. The measured host_process + `--stage` retains workspace changes while allowing access outside the view. Tested safe mode and prepared VM rootfs block outside reads/writes. An OCI writable workspace mount changes host files directly, so its speed is not a comparison with identical staging semantics.**

| Need | Selection implication |
|---|---|
| Only stage workspace changes | Staging alone does not restrict host access |
| Block reads/writes outside the view | Use a verified isolation configuration |
| OCI writable workspace mount | Host writes follow the mount grant |

## Motivation {#motivation}

Every performance result must correspond to its actual boundary. Negative controls and final host-content checks distinguish successful requests from host mutation.

## Experiment design {#interpretation}

Fresh fixtures probe absolute paths, symlinks, /proc/self/root, traversal, Unix sockets and lower-workspace aliases. Host/staged deliberately provide negative controls. Inspect original host content and Bundle observed isolation/staging, not just syscall return values. The isolation matrix uses a prepared tool rootfs; other rootfs and path grants require their own checks.

The matrix uses pinned Linux/x86_64 artifacts and declared configurations from 2026-10-04, rather than asserting every newer default. See [executor boundaries](../security/executor-boundaries.md) for current defaults. Matching macOS workloads are unmeasured.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

| Profile | Host outside readable | Host outside written | Host lower alias written | Workspace staged |
|---|---|---|---|---|
| host | True | True | True | False |
| staged / host_process | True | True | True | True |
| safe | False | False | False | True |
| vm | False | False | False | True |
| container | False | False | True | False |

### Analysis

Safe/VM lower-alias writes can return success while landing in stage and leaving host lower unchanged. Syscall status alone would misclassify this. OCI writes through its explicitly writable mount; that is a declared grant, not a promise that all mounts stage changes. Host/staged can read/write outside fixture paths and connect the socket; they are not counted as safe profiles.

Direct-socket denials appear in [network](network.md); submission/conflicts/interruption recovery in [apply](apply.md). This matrix measures correctness rather than denial speed.

### Scope {#acceptance}

These checks cover the listed file-access and exit cases, not kernel-vulnerability or escape audits. Mounts, network and rootfs configuration determine boundaries; standalone stage is not a complete sandbox.

### Downloads and reproduction {#run}

[Derived table CSV](isolation-tests.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

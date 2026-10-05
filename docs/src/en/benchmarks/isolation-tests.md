# Isolation effectiveness: actual host effects

## Main conclusions {#conclusions}

Choose the isolation boundary before comparing speed. Standalone `--stage` retains workspace changes while allowing access outside the view. Tested safe mode and prepared VM rootfs block outside reads/writes. An OCI writable workspace mount changes host files directly, so its speed is not a comparison with identical staging semantics.

## Motivation {#motivation}

Every performance result must correspond to its actual boundary. Negative controls and final host-content checks distinguish successful requests from host mutation.

## Experiment design {#interpretation}

Fresh fixtures probe absolute paths, symlinks, /proc/self/root, traversal, Unix sockets and lower-workspace aliases. Host/staged deliberately provide negative controls. Inspect original host content and Bundle observed isolation/staging, not just syscall return values. VM uses a prepared tools rootfs here, unlike filesystem timing with host rootfs `/`.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

## Data and analysis {#results}

| Profile | Host outside readable | Host outside written | Host lower alias written | Workspace staged |
|---|---|---|---|---|
| host | True | True | True | False |
| staged | True | True | True | True |
| safe | False | False | False | True |
| vm | False | False | False | True |
| container | False | False | True | False |

### Analysis

Safe/VM lower-alias writes can return success while landing in stage and leaving host lower unchanged. Syscall status alone would misclassify this. OCI writes through its explicitly writable mount; that is a declared grant, not a promise that all mounts stage changes. Host/staged can read/write outside fixture paths and connect the socket; they are not counted as safe profiles.

Direct-socket denials appear in [network](network.md); submission/conflicts/interruption recovery in [apply](apply.md). This matrix measures correctness rather than denial speed.

### Scope {#acceptance}

These checks cover the listed file-access and exit cases, not kernel-vulnerability or escape audits. Mounts, network and rootfs configuration determine boundaries; standalone stage is not a complete sandbox.

### Data sources and reproduction {#run}

[Configuration and sampling](methodology.md#product-v1) · [Manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv) · [Samples CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw evidence](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)

Reproduction commands and prerequisites are in the [technical methodology record](../design/benchmark-methodology-evidence.md).

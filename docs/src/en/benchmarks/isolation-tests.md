# Isolation effectiveness: actual host effects

Staging retains workspace changes but alone still permits access outside that view. Safe, a prepared VM rootfs and OCI blocked the outside read/write/socket probes; an explicitly writable OCI workspace mount changes the host directly.

## Motivation

Every performance result must correspond to its actual boundary. Negative controls and final host-content checks distinguish successful requests from host mutation.

## Experiment design {#interpretation}

Fresh fixtures probe absolute paths, symlinks, /proc/self/root, traversal, Unix sockets and lower-workspace aliases. Host/staged deliberately provide negative controls. Inspect original host content and Bundle observed isolation/staging, not just syscall return values. VM uses a prepared tools rootfs here, unlike filesystem timing with host rootfs `/`.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

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
## Limits and next measurements {#acceptance}

These probes are not penetration testing, kernel-exploit coverage or a complete security proof. Shares, rootfs contents, policies and executor configuration change the boundary. Credentials, all mount/rename combinations and malicious kernel exploitation are not covered. Human semspec approval remains distinct from test success.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites isolation --samples 1 --warmups 0
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.

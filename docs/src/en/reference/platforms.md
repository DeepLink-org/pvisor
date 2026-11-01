# Platforms and executor support

Choose an executor for your platform, then check its prerequisites. Host tasks are a quick starting point on Linux; choose a VM when you need Linux userspace or a separate kernel. VMs on Apple Silicon use Linux aarch64 userspace.

After installation, check that the controls you need are active. The executor observations in each Run Bundle identify that run's effective boundaries.

## Mechanisms and prerequisites today

Prepare the environment using these prerequisites, then validate the features you need with a small task and its Bundle.

| Platform | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | FUSE for staging; usable user namespaces and Landlock for sandbox | Native `crun`/`runc` and a matching static Linux binary | `/dev/kvm`; musl release embeds the guest kernel |
| Linux arm64 | Source paths exist; check build and test evidence first | A matching `linux/arm64` injected binary | Host/build KVM support and a matching rootfs; do not infer support from the x86_64 wheel |
| Apple Silicon macOS | macFUSE 5.4.0+ FSKit for staging; Seatbelt for files and network | Native OCI Linux-container path unsupported | HVF, Hypervisor signing and a Linux arm64 rootfs/image; no macFUSE needed |
| Windows x86_64 | Not ported | Not ported | Source-built `vm-run` / WHP; limited profile below |
| Intel macOS | No current release support promise | No promise | No promise |

The `container` column describes the native Job OCI executor (`--executor container`). In the [Docker-style container commands](cli.md#container-commands), `pvisor run` uses independent Linux VMs, with implementation paths on Linux x86_64/KVM and Apple Silicon/HVF. It does not use the OCI runtime in this column or implement the Docker Engine API.

Current wheels target Linux x86_64 and macOS arm64. Source branches do not establish released artifacts or maturity. Check `pvisor --version`, run a small task, and inspect its Bundle. See [installation](../start/installation.md).

## Feature sets and critical paths across three platforms

Windows x86_64 now has a limited source-built entry point: `pvisor vm-run`.
Linux x86_64/KVM, Apple Silicon macOS/HVF and Windows x86_64/WHP share its
arguments, guest configuration and authenticated exit protocol. It boots a Linux
guest directly without a Job, Bundle or container instance. Rootfs writes persist
directly; stage/review/apply is not involved. Other Windows product commands are
not ported, and there is no Windows wheel. Intel macOS is outside this profile.

| Feature | Linux x86_64 | Apple Silicon macOS | Windows x86_64 |
| --- | --- | --- | --- |
| `vm-run`, 1/2 vCPU, RAM configuration, authenticated exit | KVM | HVF; signed binary required | WHP feature enabled followed by reboot; nested hosts must expose virtualization |
| Guest architecture | Linux x86_64 | Linux aarch64 | Linux x86_64 |
| Directory rootfs and extra read-only share | Supported, common path without DAX | Supported, common path without DAX | Supported; synthesized Linux mode/uid/gid, host ACLs remain effective |
| Host Jobs, OverlayFS, review/apply/drop | FUSE, namespaces/Landlock | macFUSE FSKit, Seatbelt | Not ported; corresponding CLI commands rejected |
| Docker-style VM containers and network policy | Implementation and separate contracts | Implementation; each capability needs HVF evidence | Not ported; VM network/vsock return Unsupported |
| Linux host-root `stage`, `:stage` binds, native OCI | Linux-specific paths | No Linux host-root/native OCI; staged-bind equivalence is not implied | Unsupported |
| VM snapshots, restore, cold RAM | Dedicated APIs/tests, with backing/profile restrictions | Dedicated APIs/tests, with HVF/backing restrictions | Unsupported; distinct from pause/resume |
| Gateway, replay, daemon | Per-interface contracts; sandbox daemon needs KVM/cgroup v2 | Host/Gateway/replay paths; daemon cache/memory components only | Excluded from this CLI profile |

The common chain is `pvisor vm-run → pvisor_vm::api::VmBuilder → platform VMM →
virtio-fs → Rust PID 1 supervisor → Linux workload → token-authenticated exit`.
Linux uses KVM ioctls; macOS uses HVF with the Hypervisor entitlement; Windows
uses WHP partitions, APIC/PIT/TSC and Win32 waitable handles. WHP must preserve
Linux wire errno, flags and ABI widths rather than substitute host CRT values.
Callers must exclusively manage the rootfs; Windows does not provide full Linux
POSIX metadata or a security promise against hostile host directory mutation.

| Input/variable | Critical path and observation |
| --- | --- |
| `--cpus`, `--memory` | vCPU startup/interrupts and memory pressure; allowed ranges 1–64 and 1–65536 MiB, further constrained by firmware/host; this suite checks only 1/2 CPUs and 256 MiB |
| `--firmware` | Explicit host-loadable `.so`/`.dylib`/`.dll`; omission requires an embedded kernel; Windows currently needs a DLL; use trusted repository firmware matching guest architecture |
| `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` | Build-time firmware/kernel inputs, distinct from runtime `--firmware`; product builds select these inputs |
| `--rootfs`, `--readonly` | Separate non-overlapping directories; rootfs writes persist, read-only share mounts at `/readonly`; absolute UTF-8 host paths and an absolute Linux guest executable |
| Host/guest page sizes, filesystem, case handling, path length, ACLs | mmap, rename, cached inodes and errno; these cases do not establish universal POSIX equivalence |
| `--seed`, `--rounds`, `--timeout` (test driver) | Fixed data sequence, repeated VM lifecycles and per-command deadline; scheduling and wall time vary |

`vm-run` provides no networking, interactive stdin, separate task stdio,
container lifecycle, snapshots or Job policies. Output uses the VM console and
may include kernel logs. Workload exit codes propagate; argument errors return
2, preparation/runtime errors 1, and authentication failure/abnormal VM exit 125.
An external supervisor owns deadlines and termination; the driver below kills
the process tree on timeout and retains failure evidence.

## Shared VM cases and three-platform CI

The same [guest workload](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_guest.rs) and
[CLI driver](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_cli_contract.py) exercise all backends without
an SDK or simulated guest results. Three default rounds each check
`(1 CPU, exit 0)`, `(2, 0)`, `(2, 7)`, `(1, 255)`: 12 actual boots. Each boot
runs 8 threads with 32 iterations of 4–8 KiB writes, fsync, rename, read and
unlink, and touches 16 MiB of memory. It also checks online CPUs, a 100 ms timer,
read-only rejection, directory rename with cached descendants, and final host
results. Results must match the seed/CPU/exit marker exactly; exit status alone
cannot pass. The loose timer upper bound checks liveness, not performance.

| CI path | Preparation and execution | Failure and evidence |
| --- | --- | --- |
| `ci.yml` / Rust Test / Linux / pvisor | Build product and x86_64 musl guest, check `/dev/kvm` access, execute shared driver | `vm-cli-Linux`: per-case logs, rootfs and JSON; failure blocks main CI |
| `ci.yml` / Rust Test / macOS / pvisor | Build/sign product and aarch64 musl guest, execute the same driver on real HVF | `vm-cli-macOS`, identical assertions and gate |
| `windows-vm.yml` / native + firmware + whp | Native MSVC `pvisor --bin` build/parser tests; Linux builds DLL and same x86_64 guest; Windows executes same driver | `windows-vm-whp-log` includes shared-case evidence; unavailable WHP, reboot requirements and timeouts fail without skips |

Existing Windows `whp_smoke` still checks pause/resume, cleanup after rejected
ready callbacks and legacy exit ioctl rejection. Native contracts and actual VM
execution are separate evidence; compilation cannot substitute for execution.
Windows CLI builds select `cargo build --locked -p pvisor-cli --bin pvisor` and
tests select `mise run test -- pvisor-cli -- --lib --bin pvisor`. TUI/replay and
existing Unix integration tests are outside the Windows build selection.

Manual execution requires an executable/signed `pvisor`, matching static guest
and firmware:

```bash
# Linux: x86_64-unknown-linux-musl; Apple Silicon: aarch64-unknown-linux-musl.
mise exec -- rustc --edition=2021 --target aarch64-unknown-linux-musl -C linker=rust-lld tests/vm_guest.rs -o /tmp/vm-guest
python3 tests/vm_cli_contract.py --binary target/debug/pvisor --guest /tmp/vm-guest --output target/vm-cli-evidence
# Windows uses the same driver with --firmware C:/firmware/libkrunfw.dll.
# --output must be new; --rounds 30 expands coverage to 120 actual boots.
```

Determinism here means consistent exit status, file results, read-only boundaries
and cleanup for fixed inputs, without promising identical scheduling/timing.
This is bounded correctness traffic, not long-running stability or production
density certification. CI configuration is not a passing run: inspect actual
results for the corresponding commit. See the [platform review record](https://github.com/DeepLink-org/pvisor/blob/main/tests/PLATFORM_REVIEW.md)
for findings and remaining coverage.

## Single-node daemon {#daemon}

The sandbox API requires Linux x86_64/KVM and delegated cgroup v2; macOS HVF and Linux arm64 are outside this API's supported scope. `prepare-image` publishes templates in the shared compressed image pool, with the Rust guest engine as the default for new templates. Official OpenSandbox/E2B SDK core flows have real-VM validation; full protocol conformance and production density are not guaranteed. See the [daemon guide](../guides/daemon/index.md) for deployment.

The same executable's cache and memory-pool components support Linux and Apple Silicon macOS without enabling the sandbox API. macOS offers compression or explicit `physical-pages` sharing; see [memory optimization](../design/memory-optimization/index.md) for prerequisites and failure boundaries.

## Verify capability evidence

- Check `safety.filesystem_read_non_bypassable` and `filesystem_write_non_bypassable` separately, not just whether changes are staged.
- Check `safety.network_non_bypassable` and the driver; selective Linux proxies and container host networking do not become mandatory boundaries just because of the executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect `resources.effective` and `limitations`. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `docs/src/zh/cases/06-stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

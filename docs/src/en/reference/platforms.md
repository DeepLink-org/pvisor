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

## Binary inventory: is one executable enough? {#binaries}

**One `pvisor.exe` is enough as the host program for `vm-run`; it is insufficient
for the full pVisor workflow.** Windows also needs matching `libkrunfw.dll`, a
caller-prepared Linux rootfs and its workload. The full Linux/macOS product has
not been merged into a single executable. Windows Daemon/TUI/replay are not
ported. Complete Linux/macOS installations contain four host programs.

| Program and owner | Responsibility and call chain | Linux x86_64 | Apple Silicon macOS | Windows x86_64 |
| --- | --- | --- | --- | --- |
| `pvisor` / `pvisor-cli` | Command entry; self-launches ordinary Job owners; `vm-run` executes a VM directly in its process | Full CLI built and packaged | Full CLI built and packaged; VM executables need Hypervisor signing | Only `pvisor.exe` with `vm-run`, help/version is built |
| `pvisor-tui` / `pvisor-cli` | `pvisor tui`, Job `--tui/--ask`, interactive resume; TUI launches adjacent `pvisor` and owns the PTY, terminal and review panels | Built and packaged | Built and packaged | Not ported or built; these options are absent from `vm-run` |
| `pvisor-replay` / `pvisor-cli` | `pvisor replay` dispatch; managed replay invokes adjacent `pvisor` using existing Job/trajectory contracts | Built and packaged | Built and packaged | Not ported or built |
| `pvisor-daemon` / separate crate | `pvisor daemon` dispatch; shared services and `--vm-memory-pool auto` discovery/startup; explicit cache/sandbox services | Built and packaged; sandbox API needs KVM/cgroup v2 | Built and packaged; cache/pool components, no sandbox API | Not ported or built; no Windows local-service contract |

`pvisor-guest` is the Rust PID 1 supervisor inside Linux guests, normally embedded
as a `pvisor-vm` payload. Cross-compiling it on Windows does not produce a host
`pvisor-guest.exe` to install. `whp_smoke.exe` is a CI VM API probe, not a Daemon
or product companion. `containerd-shim-pvisor-v2` serves Linux containerd
integration, outside the common four-program installation. Firmware and Linux
workloads cannot be replaced with Windows executables either.

Choose components by the functions used:

| Usage | Required host components | Current Windows conclusion |
| --- | --- | --- |
| Execute `vm-run` against a prepared rootfs; inspect exit status and files | `pvisor`; trusted firmware DLL on Windows | This PR's profile; no Daemon/TUI/replay required |
| Noninteractive `spawn`, review/apply/drop | Full `pvisor` plus selected executor/drivers; ordinary Jobs need no resident Daemon | Unsupported; the runtime is absent from this executable, and companion stubs cannot supply it |
| `spawn --tui/--ask`, `pvisor tui`, interactive resume | Full `pvisor` + `pvisor-tui` + working PTY | Unsupported |
| `pvisor replay` | `pvisor-replay`; managed execution also needs full `pvisor` | Unsupported |
| Automatic shared memory pool, local cache service or sandbox API | Corresponding CLI/client + `pvisor-daemon`; prerequisites vary by service | Unsupported; reaching a remote Linux daemon API does not establish a Windows local daemon port |

Complete wheel/native archive builds select all four host programs. Wheel targets
are Linux x86_64 musl and macOS arm64. Windows CI explicitly selects `--bin pvisor`
to verify the VM CLI profile; it does not establish a complete Windows package
or a successful all-target `cargo build -p pvisor-cli`. Keep Unix companions
adjacent and from the same version; copying only the main binary leaves the
corresponding commands unavailable. Source evidence:
[binary inventory and build targets](https://github.com/DeepLink-org/pvisor/blob/main/scripts/packaging/stage_wheel_binaries.py),
[companion discovery and trust checks](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/companions.rs),
[automatic pool discovery](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/cli/daemon.rs).

## Implementation differences and full-workflow acceptance {#implementation-parity}

| Layer | Linux | macOS | Current Windows implementation/gap |
| --- | --- | --- | --- |
| CLI and process ownership | Full command tree, self-launched Job owners, signals and descendant cleanup | Same Job/companion structure, with macOS terminal/resource limits | Minimal command tree dispatches only into shared `vm-run`; no Job owner, control socket or Job state records |
| Companion installation/dispatch | Adjacent discovery, Unix uid/mode checks and `exec` dispatch | Same checks; resolves Homebrew entry to its real installation | No discovery/dispatch; a full port needs `.exe` lookup, ACL trust, process startup and exit propagation contracts |
| Terminal/interaction | PTY, terminal foreground ownership and signals | PTY and terminal restoration; separate TUI executable | Windows console/PTY lifecycle is not integrated; `vm-run` console output does not establish TUI compatibility |
| Shared services and IPC | Unix sockets, same-user authentication, locks and readiness; sandbox also requires cgroup/KVM | Unix IPC for shared services; cache/pool supported, sandbox API unsupported | Equivalent IPC, endpoint ACLs, locks, cancellation and service cleanup are undefined; a bootable WHP guest does not validate them |
| VM/filesystem | KVM + Unix virtio-fs; full runtime can also select overlay/network configurations | HVF + Unix virtio-fs; signing, page-size and host-permission differences | WHP + Win32 directory shares; Linux wire ABI conversion, synthetic metadata; overlay/network/snapshots remain Unsupported |
| Delivery | Four-program product package and separate integration artifacts | Four-program product package with Hypervisor signing | Limited source/CI profile; no complete Windows wheel or four-program package |

Full Windows pVisor support requires actual builds/installation of the programs,
adjacent discovery and version/permission checks, real CLI→TUI→CLI,
CLI→replay→CLI and CLI→Daemon calls, cancellation/deadlines/descendant cleanup,
and readiness/failure-recovery tests for the services used. Validate Job
JSON/Bundles, review/apply, terminal interaction and service protocols separately
before extending the support matrix. Named stubs, `--help` checks alone, or this
VM workload alone cannot establish those capabilities.

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

### Scope of recorded execution evidence

[PR #92's first CI run](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)
for commit `5b320104` passed Windows native MSVC builds/contracts, WHP API smoke
and the shared CLI workload. Linux/KVM passed the shared CLI workload. macOS
built all four product programs and passed its separate CLI tests, but the
shared VM workload failed during HVF `VmCreate` before executing the guest.
This is not a three-platform pass. Local Apple Silicon results (12 plus 120
successful HVF boots) describe a different host and cannot replace that runner's
result. That run's macOS core shard also had a replay-test timeout, so main CI
cannot be reported green. This snapshot describes that commit only; subsequent
commits require fresh checks.

## Single-node daemon {#daemon}

The sandbox API requires Linux x86_64/KVM and delegated cgroup v2; macOS HVF and Linux arm64 are outside this API's supported scope. `prepare-image` publishes templates in the shared compressed image pool, with the Rust guest engine as the default for new templates. Official OpenSandbox/E2B SDK core flows have real-VM validation; full protocol conformance and production density are not guaranteed. See the [daemon guide](../guides/daemon/index.md) for deployment.

The same executable's cache and memory-pool components support Linux and Apple Silicon macOS without enabling the sandbox API. macOS offers compression or explicit `physical-pages` sharing; see [memory optimization](../design/memory-optimization/index.md) for prerequisites and failure boundaries.

## Verify capability evidence

- Check `safety.filesystem_read_non_bypassable` and `filesystem_write_non_bypassable` separately, not just whether changes are staged.
- Check `safety.network_non_bypassable` and the driver; selective Linux proxies and container host networking do not become mandatory boundaries just because of the executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect `resources.effective` and `limitations`. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `docs/src/zh/cases/06-stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

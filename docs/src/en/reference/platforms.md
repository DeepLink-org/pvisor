# Platforms and executor support

Linux x86_64 and Apple Silicon macOS have full CLI installations. Windows x86_64
currently supports source-built `pvisor vm-run`; Jobs, TUI, replay and the local
Daemon are not ported.

One `pvisor.exe` covers this VM entry point, with `libkrunfw.dll`, a Linux rootfs
and its workload still required. CI runs the shared VM workload on Linux/KVM
and Windows/WHP. Hosted macOS CI retains builds and conventional tests without
running real HVF cases; the same workload remains available manually on an
Apple Silicon Mac with working HVF.

## Installed components {#binaries}

Complete Linux/macOS wheels and native archives contain four host programs from
the same version, installed together. Ordinary noninteractive Jobs need no
resident Daemon. Other components are invoked by the features that use them.

| Program and owner | Used for | Windows status |
| --- | --- | --- |
| `pvisor` / `pvisor-cli` | Command entry; self-launches ordinary Job owners; `vm-run` executes a VM in the current process | Only `pvisor.exe` with `vm-run`, help/version |
| `pvisor-tui` / `pvisor-cli` | `pvisor tui`, `spawn --tui/--ask`, interactive resume; launches adjacent `pvisor` and owns the PTY, terminal and review panels | Not ported |
| `pvisor-replay` / `pvisor-cli` | `pvisor replay`; managed replay calls adjacent `pvisor` using Job/trajectory contracts | Not ported |
| `pvisor-daemon` / separate crate | `pvisor daemon`, cache/sandbox services; `--vm-memory-pool auto` discovers or starts the shared pool through it | Not ported |

Copying only the main program leaves companion-dependent commands unavailable.
Windows CI selects `--bin pvisor`; all targets of `cargo build -p pvisor-cli` and
a complete Windows package are unsupported. Wheel targets are Linux x86_64 musl
and macOS arm64. See [installation](../start/installation.md).

`pvisor-guest` is the Linux PID 1 supervisor embedded in `pvisor-vm`. Cross-compiling it on
Windows still produces a Linux payload; no host `pvisor-guest.exe` is needed.
`whp_smoke.exe` is a CI probe. `containerd-shim-pvisor-v2` is delivered separately
for Linux containerd integration. Guest workloads must be Linux programs.

The [binary inventory and build targets](https://github.com/DeepLink-org/pvisor/blob/main/scripts/packaging/stage_wheel_binaries.py)
list the programs. Adjacent-program permission checks and dispatch live in
[companion discovery and trust checks](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/companions.rs);
pool calls live in [automatic pool discovery](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/cli/daemon.rs).

## Mechanisms and prerequisites today

| Feature | Linux x86_64 | Apple Silicon macOS | Windows x86_64 |
| --- | --- | --- | --- |
| `vm-run` | KVM, readable/writable `/dev/kvm`; Linux x86_64 guest | HVF, Hypervisor signing; Linux aarch64 guest, no macFUSE needed | WHP enabled followed by reboot; nested hosts must expose virtualization; Linux x86_64 guest |
| Host Jobs (`spawn`), OverlayFS, review/apply/drop | FUSE for staging; user namespaces and Landlock for sandbox | macFUSE 5.4.0+ FSKit for staging; Seatbelt for file/network controls | Not ported |
| Native OCI Jobs (`container` / `--executor container`) | `crun`/`runc` and a matching static Linux injected binary | Unsupported | Unsupported |
| Docker-style VM containers and network policy | Implementation and separate contracts | Implementation; each capability needs HVF validation | Not ported |
| Linux host-root `stage`, `:stage` binds | Linux-specific paths | No Linux host-root; staged-bind equivalence with Linux is unestablished | Unsupported |
| VM snapshots, restore, cold RAM | APIs/tests with backing/profile restrictions | APIs/tests with HVF/backing restrictions | Unsupported; pause/resume only |
| Gateway | Depends on executor and configuration | Host execution path exists | Excluded from this CLI |

[Docker-style container commands](cli.md#container-commands) such as `pvisor run`
use one Linux VM per container. They are separate from native OCI Jobs and do
not implement the Docker Engine API. Linux arm64 has source paths requiring
host-specific build/KVM evidence and a matching rootfs; OCI also needs a
`linux/arm64` injected binary. The x86_64 wheel does not cover arm64. Intel macOS
has no current release support promise and is outside `vm-run`.

After installation, run `pvisor --version`. Inspect effective Job controls in
the Run Bundle; check `vm-run` results through exit status and host files.

## Platform implementations {#implementation-parity}

All three platforms share `vm-run` arguments, GuestConfig and authenticated exit
handling. The execution chain is
`pvisor vm-run → pvisor_vm::api::VmBuilder → platform VMM →
virtio-fs → Rust PID 1 supervisor → Linux workload → token-authenticated exit`.

| Layer | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CLI/processes | Full command tree, Job owners, signals and descendant cleanup | Same Job/companion structure, with macOS terminal/resource limits | Minimal command tree calls shared `vm-run`; no Job owner, control socket or Job state records |
| Companions | Adjacent discovery, Unix uid/mode checks, `exec` dispatch | Same checks; resolves Homebrew entry to its real installation | No dispatch; a full port needs `.exe` lookup, ACL trust and process startup/exit propagation |
| Terminal | PTY, foreground ownership and signals | PTY, terminal restoration; separate TUI | Windows console/PTY lifecycle is not integrated |
| Shared services | Unix sockets, same-user authentication, locks and readiness; sandbox also needs cgroup/KVM | Unix IPC; cache/pool supported, sandbox API unsupported | IPC, endpoint ACLs, locks, cancellation and service cleanup are not ported; reaching a remote Linux API does not supply a local service |
| VM/filesystem | KVM ioctls, Unix virtio-fs; full runtime also has overlay/network configurations | HVF, Unix virtio-fs; signing, page-size and host-permission differences | WHP partitions, APIC/PIT/TSC, Win32 waitable handles and directory shares; Linux wire errno, flags and ABI widths require conversion |

The common directory-share path disables DAX. Windows synthesizes Linux
mode/uid/gid; host ACLs still apply, and POSIX metadata semantics are incomplete.
Callers must exclusively manage rootfs and prevent untrusted host processes from
mutating shared trees.

A full Windows port also needs adjacent installation and version/permission
checks, real CLI→TUI→CLI, CLI→replay→CLI and CLI→Daemon calls. Tests must cover
terminal restoration, cancellation/deadlines, descendant cleanup, service
readiness and failure recovery, then check Job JSON/Bundles, review/apply and
service protocols separately. `--help` or VM workload results alone do not
validate those calls.

## Feature sets and critical paths across three platforms

`vm-run` writes directly to caller-owned rootfs. It creates no Job, Bundle, stage
or container instance and provides no network/vsock, interactive stdin, separate
task stdio, container lifecycle, snapshots or Job policies.
Output uses the VM console and may include kernel logs. Workload exit codes
propagate; argument errors return 2, preparation/runtime errors 1, and exit
authentication failure or abnormal VM termination 125. The external caller owns
deadlines and process termination.

| Input/variable | Effect and requirements |
| --- | --- |
| `--cpus`, `--memory` | vCPU startup, interrupts and memory pressure; allowed ranges 1 to 64 vCPU and 1 to 65536 MiB, further constrained by firmware/host; this suite covers only 1/2 CPUs and 256 MiB |
| `--firmware` | Trusted host `.so`/`.dylib`/`.dll` matching guest architecture; omission requires an embedded kernel. Musl releases embed the kernel; Windows currently needs an explicit DLL |
| `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` | Firmware/kernel inputs selected by product builds, distinct from runtime `--firmware` |
| `--rootfs`, `--readonly` | Non-overlapping directories and absolute UTF-8 host paths; the read-only share mounts at `/readonly`, and the guest executable needs an absolute Linux path |
| Host/guest page sizes, filesystem, case handling, path length, ACLs | Affect mmap, rename, cached inodes and errno; these cases do not cover all POSIX differences |
| `--seed`, `--rounds`, `--timeout` (test driver) | Control data sequence, repeated lifecycles and per-command deadline; scheduling and elapsed time vary |

## Shared VM cases and three-platform CI

### Execution record

[PR #92's first CI run](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)
for commit `5b320104` has not passed all checks.

| Platform | Builds and unit tests | Shared VM workload |
| --- | --- | --- |
| Windows | Native MSVC builds/contracts and WHP API smoke passed | WHP passed |
| Linux | Workload used the product build | KVM passed |
| macOS | Four product programs built; separate CLI tests passed; core shard also had a replay-test timeout | HVF `VmCreate` failed before guest startup |

A separate local Apple Silicon host passed 12 plus 120 HVF boots. Subsequent
commits require fresh CI checks. See the [platform review record](https://github.com/DeepLink-org/pvisor/blob/main/tests/PLATFORM_REVIEW.md)
for findings and untested cases.

### Workload and result checks

The [CLI driver](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_cli_contract.py)
executes the same [guest workload](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_guest.rs)
through the actual `pvisor` program. Three default rounds each check
`(1 CPU, exit 0)`, `(2, 0)`, `(2, 7)`, `(1, 255)`: 12 boots. Each boot runs
8 threads, each doing 32 iterations of 4 to 8 KiB writes, fsync, rename, read
and unlink, and touches 16 MiB of memory in total. It also checks online CPUs,
a 100 ms timer, read-only rejection and directory rename with cached descendants.

The host checks exit status, the exact seed/CPU/exit result, an unchanged
read-only fixture and an empty work directory. Missing virtualization, firmware
errors and timeouts fail. On timeout the driver terminates and reaps the process
tree, retaining per-case logs, commands, hashes, rootfs and JSON. The timer upper
bound checks liveness. Determinism means consistent exit status and file results
for the same input; scheduling/timing may vary. Hours-long stability and
production density remain untested.

### CI configuration and manual execution

GitHub's arm64 macOS runners [do not support nested virtualization](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#limitations-for-arm64-macos-runners).
Real HVF cases are removed from hosted CI. macOS builds and conventional tests
continue; their results do not establish real HVF acceptance.

| CI path | Preparation and execution | Artifact |
| --- | --- | --- |
| `ci.yml` / Rust Test / Linux / pvisor | Build product and x86_64 musl guest, check `/dev/kvm`, execute shared driver | `vm-cli-Linux` |
| `ci.yml` / Rust Test / macOS / pvisor | Build/sign product and run conventional tests; no real HVF workload | No VM execution artifact |
| `windows-vm.yml` / native + firmware + whp | MSVC builds `pvisor --bin` and tests parser; Linux builds DLL and x86_64 guest; Windows executes same driver | `windows-vm-whp-log` |

Existing Windows `whp_smoke` still checks pause/resume, rejected-ready-callback
cleanup and legacy exit ioctl rejection. Windows CLI builds use
`cargo build --locked -p pvisor-cli --bin pvisor`; tests use
`mise run test -- pvisor-cli -- --lib --bin pvisor`. Existing Unix integration
tests are outside that selection. Failed cases fail main CI; unavailable
WHP or a required reboot also fail.

Manual execution needs an executable `pvisor`, matching static guest and
firmware; macOS also requires signing.

```bash
# Linux: x86_64-unknown-linux-musl; Apple Silicon: aarch64-unknown-linux-musl.
mise exec -- rustc --edition=2021 --target aarch64-unknown-linux-musl -C linker=rust-lld tests/vm_guest.rs -o /tmp/vm-guest
python3 tests/vm_cli_contract.py --binary target/debug/pvisor --guest /tmp/vm-guest --output target/vm-cli-evidence
# Windows uses the same driver with --firmware C:/firmware/libkrunfw.dll.
# --output must be new; --rounds 30 expands coverage to 120 actual boots.
```

## Single-node daemon {#daemon}

The sandbox API requires Linux x86_64/KVM and delegated cgroup v2; macOS HVF and Linux arm64 are outside this API's supported scope. `prepare-image` publishes templates in the shared compressed image pool, with the Rust guest engine as the default for new templates. Official OpenSandbox/E2B SDK core flows have real-VM validation; full protocol conformance and production density are not guaranteed. See the [daemon guide](../guides/daemon/index.md) for deployment.

The same executable's cache and memory-pool components support Linux and Apple Silicon macOS without enabling the sandbox API. macOS offers compression or explicit `physical-pages` sharing; see [memory optimization](../design/memory-optimization/index.md) for prerequisites and failure boundaries.

## Verify capability evidence

- Check `safety.filesystem_read_non_bypassable` and `filesystem_write_non_bypassable` separately, not just whether changes are staged.
- Check `safety.network_non_bypassable` and the driver; selective Linux proxies and container host networking do not become mandatory boundaries just because of the executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect `resources.effective` and `limitations`. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `docs/src/zh/cases/06-stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

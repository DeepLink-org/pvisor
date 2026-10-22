# Podman coverage expansion review

This is a working audit of the pinned sources in `UPSTREAM.md`, not a completed
coverage claim. The imported corpus has 77 system `.bats` files and 144 e2e
`*test.go` files (including upstream runner/helper files). A source file with
one adapted case is only partially covered. Active counts describe executable
pVisor contracts, not the number of unchanged upstream cases passing.

## Remaining supported behavior to audit and port

| Source families | Remaining work | State |
| --- | --- | --- |
| `075-exec.bats`, `exec_test.go`, `run_exit_test.go`, `run_passwd_test.go` | Error codes, live exec cleanup, current guest passwd, inherited identity, umask, literal argv and binary/large I/O | 25 process/identity/image contracts passed in `e2e/process_test.go`, within the full 101-case profile; remaining identity/group/error variants still need audit |
| `010-images.bats`, `images_test.go`, `rmi_test.go`, `pull_test.go` | Full/short image selection, force/batch removal, filtering, native/foreign platform pull and cache identity | Existing coverage is partial; platform/ref/filter variants still pending |
| `030-run.bats`, `run_env_test.go`, `run_entrypoint_test.go`, `run_working_dir_test.go` | Defaults, environment import/removal, entrypoint/CMD precedence, workdir symlinks and identity metadata | Existing coverage is partial; inspect each supported case before concluding |
| `040-ps.bats`, `045-start.bats`, `050-stop.bats`, `055-rm.bats`, corresponding e2e files | Prefix selection, batch argument errors, repeated lifecycle, signal spelling and generation isolation | Existing coverage is partial; more variants pending |
| `035-logs.bats`, `logs_test.go` | Concurrent follow readers, output around stop/restart, large/binary stderr, empty logs | Existing coverage is partial; options absent from CLI remain excluded |
| `160-volumes.bats`, `run_volume_test.go` | Bind metadata/permissions, copy-up metadata/symlinks, volume reuse and mount precedence | Existing coverage is partial; no private store inspection |
| `450-interactive.bats`, `032-sig-proxy.bats`, terminal cases in `exec_test.go` | Real controlling-terminal input/EOF and foreground signal forwarding | Pending; investigate reuse of existing Python PTY harness rather than introducing a dependency |
| `500-networking.bats`, `run_networking_test.go` | Supported IPv4 TCP publication syntax and lifecycle, no-device policy, public TCP/DNS egress | Partial; keep deterministic local traffic tests separate from Internet-dependent checks |
| `run_cpu_test.go`, `run_memory_test.go` | Integer vCPU count and total VM RAM | Adapt as explicit VM-profile tests; Docker cgroup quota assertions cannot be claimed |
| `001-basic.bats`, `015-help.bats`, `300-cli-parsing.bats`, `620-option-conflicts.bats`, `negative_test.go` | Registered CLI commands, unsupported flags failing explicitly, inert help and validation before side effects | Pending case audit; do not import Podman's complete global option surface |

## Explicitly unsupported: exclude without counting as passing

The current CLI declares exactly ten container commands and their documented
options. Tests requiring the following absent capabilities are excluded; original
sources remain untouched. Mixed source files still need supported-case review.

- Build/buildx, Compose, tag/push/save/load/import/export/commit/history/search,
  manifest/artifact/trust/signature operations and registry login/credentials.
- create/inspect/wait/restart/attach/cp/kill/pause/top/stats/events/diff/rename,
  prune/update, volume/network management, clone and existence convenience APIs.
- Docker/Podman Engine sockets, remote/SSH/connection APIs, system service,
  systemd/Quadlet/auto-update/healthcheck, pods/Kube/farm and CRIU checkpointing.
- Privileged/device/cap-add/cap-drop, SELinux/AppArmor/seccomp/userns mappings,
  host/Pasta/container networking, custom DNS/static IP/MAC and namespace sharing.
- Cgroup quotas/fractional CPUs, reservations/swap controls, blkio and GPU/device
  controls. pVisor's integer vCPU/total RAM contract is different.
- Logs/list formatting/filter/timestamps/tail/since options, exec detach,
  preserve-fd/group-add/secret flags, umask override, live resize/detach keys,
  UDP/IPv6 and cross-architecture CPU emulation.
- Podman-specific config/storage/transport/debug/management interfaces and
  tests that intentionally corrupt Podman's private databases or conmon state.

## Deferred for human review: cost or missing public observations

These are evaluation items, not permanent exclusions and not passing tests.

| Area | Why it needs review | What would unblock a useful test |
| --- | --- | --- |
| Anonymous-volume physical deletion | No public volume list/inspect API exposes anonymous backing identity; fresh-container isolation does not prove deletion | Approve a separate storage white-box contract, or add a public observation API in a separately scoped change |
| Custom OCI metadata combinations | Upstream builds many images with USER/WORKDIR/ENTRYPOINT/CMD/Volumes/StopSignal; pVisor has no build/tag/push and the black-box runner must not forge its catalog | Reusable externally hosted immutable fixture images, or an approved local registry fixture; evaluate before deferring individual metadata cases |
| Registry faults/auth/TLS/mirror fallback/partial downloads | Needs controlled registry/proxy/certificate fixtures; upstream transport configuration is not implemented | Review which public-registry guarantees belong in the supported profile; reuse runtime OCI unit fixtures where appropriate |
| Exact cross-VM mmap/locking coherence | Separate guest kernels do not share one page cache; close-to-open coverage does not establish coherence of open mappings | Explicit consistency/locking contract and a guest probe; do not silently convert these to ordinary read/write tests |
| Heavy image/service/toolbox suites | Fedora/systemd/build/toolchains and host kernel/cgroup prerequisites make many original fixtures inappropriate; some commands are unsupported outright | Identify the assertion worth retaining and a small portable fixture; otherwise retain as deferred with the exact source case |
| Linux/KVM host acceptance | This workstation supplies macOS ARM64/HVF only | Linux x86_64 runner with `/dev/kvm` and corresponding firmware; local guest Linux execution is not equivalent evidence |

Completion requires a final pass through the source families above and a concrete
disposition for every remaining relevant scenario. This document intentionally
keeps that work open while new batches are being executed.

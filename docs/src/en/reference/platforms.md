# Platforms and executor support

Choose an executor for your platform, then check its prerequisites. Host tasks are a quick starting point on Linux; choose a VM when you need Linux userspace or a separate kernel. VMs on Apple Silicon use Linux aarch64 userspace.

Package availability and installed controls should be checked separately. The table below describes current implementation paths; the executor observations in each Run Bundle identify its effective boundaries.

## Mechanisms and prerequisites today

Prepare the environment using these prerequisites, then validate the features you need with a small task and its Bundle.

| Platform | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | FUSE for staging; usable user namespaces and Landlock for sandbox | Native `crun`/`runc` and a matching static Linux binary | `/dev/kvm`; musl release embeds the guest kernel |
| Linux arm64 | Source paths exist; check build and test evidence first | A matching `linux/arm64` injected binary | Host/build KVM support and a matching rootfs; do not infer support from the x86_64 wheel |
| Apple Silicon macOS | macFUSE 5.4.0+ FSKit for staging; Seatbelt for files and network | Native OCI Linux-container path unsupported | HVF, Hypervisor signing and a Linux arm64 rootfs/image; no macFUSE needed |
| Windows / Intel macOS | No current release support promise | No promise | No promise |

Current wheels target Linux x86_64 and macOS arm64. Source branches do not establish released artifacts or maturity. Check `pvisor --version`, run a small task, and inspect its Bundle. See [installation](../start/installation.md).

## Single-node daemon {#daemon}

The standalone daemon's current backend requires Linux, a trusted absolute rootless Podman executable, cgroup v2 and delegated CPU/memory/PID controllers. It is separate from the native executor matrix and has no macOS HVF or native KVM integration. Images must be locally prepared with real OpenSandbox 1.1.0 execd and capability-free egress; the default upstream egress conflicts with `cap-drop=ALL`, and no end-to-end image recipe is validated. See [daemon installation](../guides/daemon/index.md) and [runtime boundaries](../guides/daemon/boundaries.md), rather than inferring SDK readiness from the native VM or wheel support rows.

## Verify capability evidence

- Check `safety.filesystem_read_non_bypassable` and `filesystem_write_non_bypassable` separately, not just whether changes are staged.
- Check `safety.network_non_bypassable` and the driver; selective Linux proxies and container host networking do not become mandatory boundaries just because of the executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect `resources.effective` and `limitations`. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `tests/semantics/stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

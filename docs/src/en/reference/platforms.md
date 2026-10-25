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

The `container` column describes the native Job OCI executor (`--executor container`). In the [Docker-style container commands](cli.md#container-commands), `pvisor run` uses independent Linux VMs, with implementation paths on Linux x86_64/KVM and Apple Silicon/HVF. It does not use the OCI runtime in this column or implement the Docker Engine API.

Current wheels target Linux x86_64 and macOS arm64. Source branches do not establish released artifacts or maturity. Check `pvisor --version`, run a small task, and inspect its Bundle. See [installation](../start/installation.md).

## Single-node daemon {#daemon}

The sandbox API requires Linux x86_64/KVM and delegated cgroup v2; macOS HVF and Linux arm64 are outside this API's supported scope. `prepare-image` publishes templates in the shared compressed image pool, with the Rust guest engine as the default for new templates. Official OpenSandbox/E2B SDK core flows have real-VM validation; full protocol conformance and production density are not guaranteed. See the [daemon guide](../guides/daemon/index.md) for deployment.

The same executable's cache and memory-pool components support Linux and Apple Silicon macOS without enabling the sandbox API. macOS offers compression or explicit `physical-pages` sharing; see [memory optimization](../design/memory-optimization/index.md) for prerequisites and failure boundaries.

## Verify capability evidence

- Check `safety.filesystem_read_non_bypassable` and `filesystem_write_non_bypassable` separately, not just whether changes are staged.
- Check `safety.network_non_bypassable` and the driver; selective Linux proxies and container host networking do not become mandatory boundaries just because of the executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect `resources.effective` and `limitations`. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `docs/src/zh/cases/06-stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

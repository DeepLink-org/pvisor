---
status: todo
search:
  exclude: true
---

# Platforms and executor support

!!! warning "Planned"
    Engineering must provide evidence-backed maturity levels. See [Executor boundaries](../security/executor-boundaries.md) for current controls.

## Question

What maturity is evidenced for each platform, executor, and capability?

## Requirements

- Linux x86_64/arm64 and Apple Silicon macOS × host/container/VM × capabilities.
- Stable/Beta/experimental/unsupported maturity labels.
- Every label needs CI/spec/benchmark/issue evidence.

## Acceptance criteria

- Evidence for every cell.
- README maturity labels link here.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Boundaries](../security/executor-boundaries.md), [limitations](../security/known-limitations.md)

## Mechanisms and prerequisites today

This matrix describes implementation paths and release prerequisites, without assigning unverified Stable/Beta maturity.

| Platform | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | FUSE for staging; usable user namespaces/Landlock for sandbox | Native crun/runc and matching static Linux binary | /dev/kvm; musl release embeds guest kernel |
| Linux arm64 | Source paths exist; check build/test evidence | Matching linux/arm64 injected binary | Host/build KVM support and matching rootfs; no support inferred from x86_64 wheel |
| Apple Silicon macOS | macFUSE 5.4.0+ FSKit for staging; Seatbelt file/network controls | Native OCI Linux-container path unsupported | HVF, Hypervisor signing, Linux arm64 rootfs/image; no macFUSE needed |
| Windows / Intel macOS | No current release support promise | No promise | No promise |

Current wheels target Linux x86_64 and macOS arm64. Source branches do not establish released artifacts or maturity. Check `pvisor --version`, run a small task, and inspect its Bundle. See [installation](../start/installation.md).

## Verify capability evidence

- Check file-read and file-write booleans separately, not just staging.
- Check network non-bypassability and driver; selective Linux proxies and container host networking do not become mandatory by executor name.
- Container cannot satisfy `--safe` controls and rejects `--safe`. All current executors reject `--strict` due to the subprocess gap.
- Inspect resources.effective/limitations. macOS does not enforce RLIMIT_AS; ordinary host rlimits are not total process-tree budgets.

Evidence entry points: `.github/workflows/ci.yml`, `tests/semantics/stage-apply.md`, `crates/pvisor/tests/`, and `tests/test_vm_*`. Untested cells receive no maturity claim. See [boundaries](../security/executor-boundaries.md).

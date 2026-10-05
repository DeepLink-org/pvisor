# Positioning: gVisor / Firecracker / Kata

These runtimes provide execution boundaries. pVisor manages Jobs, pending changes, review and evidence above the boundary. Its current VM backend is libkrun; other runtimes' published timings are not pVisor measurements.

## Status on 2026-10-04

| Substrate | Official role | pVisor integration | Selection consideration |
|---|---|---|---|
| gVisor/runsc | Per-sandbox application kernel and OCI runtime | No accepted dedicated runsc backend; this edition validates crun only | Evaluate compatibility when reducing direct host-kernel exposure while retaining OCI |
| Firecracker | KVM microVM VMM with a small device surface | Not integrated; an installed CLI is not a pVisor executor | Candidate for existing Linux microVM provisioning infrastructure |
| Kata Containers | Lightweight VMs and guest kernels within container workflows | Not integrated or accepted; replacing a runtime executable is insufficient | Evaluate VM boundaries in existing Kubernetes/OCI clusters |
| libkrun | Linux guest VMs on KVM/HVF | Current pVisor backend with startup, lifecycle and snapshot evidence | Local VMs, file sharing and integrated execution semantics |

Sources: [gVisor](https://gvisor.dev/docs/), [Firecracker](https://firecracker-microvm.github.io/), [Kata](https://katacontainers.io/). Integration evidence: [pVisor executors](../guides/executors/index.md).

## Practical startup and task costs with complete Ubuntu {#full-ubuntu}

The new Firecracker 1.13.1 / complete Ubuntu 26.04.1 LTS comparison uses stock `7.0.0-34-generic`, the official initrd and normal systemd services. Fresh VMs from a prepared disk reach Ready in **5.64 / 6.01 seconds** P50/P95; first cloud-init boot takes **9.25 / 10.29 seconds**. Image-free pVisor hostroot takes **110 / 122 ms**, with the same 2 vCPU / 2 GiB and two host cores, N=30.

Complete repair/testing takes **4.61 s** in pVisor VM and **8.51 s** on Ubuntu; internal tools take **4.02 / 2.30 s**, with N=10 and 16 GiB VMs. This supports reduced new-environment waiting, not a roughly 50-times VMM advantage or universally faster tools. Services, kernels, tool versions, storage and staging differ; reusing VMs amortizes boot cost.

QEMU 10.2.2 q35 / microvm follow-up with the same complete Ubuntu gives startup P50 **5.43 / 7.67 s** and repair/testing **8.12 / 10.33 s**, N=10 per case and 80/80 pass. All create fresh environments; QEMU distributions remain independent of the earlier Firecracker/pVisor batches. Millisecond trimmed-reference boot results cannot replace seconds of complete-distribution startup.

[Startup](startup.md#full-ubuntu) · [Full Agent Env](agent-tasks.md#full-ubuntu) · [Protocol](methodology.md#full-ubuntu)

## Measured performance: pVisor VM / Firecracker / QEMU {#reference-comparison}

The following historical controls use trimmed kernels and static init, without booting complete Ubuntu on Firecracker/QEMU. The old Claude/VM initialization failure belongs to its prepared-directory configuration. See the [new hostroot / complete Ubuntu results](#full-ubuntu) above; the batches remain separate.

Independent reference CLIs now provide same-host Linux performance measurements; absence of pVisor integration is separate from absence of measurements. VMs use 2 vCPU and the same two-core host budget, 128 MiB for shell startup and 16 GiB for tools. Firecracker/QEMU share a kernel and ext4; pVisor uses embedded firmware and staged virtio-fs. Both QEMU microvm and q35 are measured, with configurations and failures retained.

| Runtime | First output P50 ms | Repair/tests P50 s | Codex loop P50 s |
|---|---|---|---|
| pVisor VM | 86.29 | 3.97 | 10.93 |
| Firecracker PCI | 73.74 | 2.25 | 7.83 |
| QEMU q35 | 218.12 | 1.98 | 7.69 |
| QEMU microvm | 88.10 | 1.85 | 7.67 |


pVisor startup is in the hundred-millisecond microVM range. Complete repair/testing takes about 2.1 times the reference microvm path; its Codex loop also adds about 3.3 seconds. The supported result is fast access to a VM with integrated pVisor semantics, not leading tool performance. Filesystems and guest configurations differ, so all differences cannot be attributed to libkrun. Claude passes 30/30 on each reference VM, but pVisor VM initialization times out: a remaining compatibility gap.

gVisor/Kata have no same-host performance samples and are not ranked using published numbers. Firecracker runs without jailer, with a different deployment security scope. [Complete tasks/figures](agent-tasks.md#reference-env) · [Startup](startup.md#reference-startup) · [Protocol/resources](methodology.md#reference-env)


## Acceptance

A runnable command is only the beginning. A backend must accurately report isolation, implement requested file/network/resource controls, clean up process trees, preserve Bundles and pass staging/replay contracts. Check CPU, device, disk and connection snapshot coverage independently.

No delivery date is promised for the three unintegrated backends. Future integrations need runtime-specific policy mapping and regressions. [Startup](startup.md), [memory/snapshots](vm-memory/index.md) and [isolation](isolation-tests.md) do not establish a speed ranking between substrates.

## Corrections

Submit backend versions, integration commits and test evidence to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues). Update the matrix after integration and validation.

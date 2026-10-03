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

## Acceptance

A runnable command is only the beginning. A backend must accurately report isolation, implement requested file/network/resource controls, clean up process trees, preserve Bundles and pass staging/replay contracts. Check CPU, device, disk and connection snapshot coverage independently.

No delivery date is promised for the three unintegrated backends. Future integrations need runtime-specific policy mapping and regressions. [Startup](startup.md), [memory/snapshots](vm-memory/index.md) and [isolation](isolation-tests.md) do not establish a speed ranking between substrates.

## Corrections

Submit backend versions, integration commits and test evidence to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues). Update the matrix after integration and validation.

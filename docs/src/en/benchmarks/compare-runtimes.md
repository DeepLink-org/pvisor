# Comparison: Firecracker, QEMU, gVisor and Kata

## Main conclusions {#conclusions}

**pVisor VM lightweight startup is close to Firecracker/QEMU microvm, while tool tasks are currently slower.** Same-tool repair/test P50 is **3.97 s** pVisor, **2.25 s** Firecracker and **1.85 s** QEMU microvm. Image-free pVisor returns short tasks sooner than booting complete Ubuntu by reducing full-system initialization waiting.

gVisor/Kata lack matching measurements. pVisor currently uses libkrun; independent Firecracker/QEMU benchmarks do not make them integrated pVisor executors.

## Motivation {#motivation}

An independent guest kernel, OCI workflow or system-call isolation can determine runtime choice. VMM startup, OS boot, tool execution and pVisor staging/recording costs must be distinguished.

## Experiment design {#interpretation}

Linux same-host references share two cores and 2 vCPU: lightweight startup 128 MiB, tools 16 GiB, N=30 and three warmups. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm, trimmed kernels and static init. Separate complete Ubuntu uses its distribution kernel, initrd and systemd, startup 2 GiB, Firecracker N=30 and QEMU N=10. These do not form an identical-security or identical-OS ranking.

| Base | Execution approach | pVisor status |
|---|---|---|
| libkrun | Local Linux guest VMs | Current integrated VM backend |
| Firecracker / QEMU | Independent VMMs | Reference CLIs measured; not accepted integrated executors |
| gVisor | Application kernel handles system interfaces; runsc | No matching local performance or dedicated-backend acceptance |
| Kata | VMs supporting container workflows | No accepted integration or matching measurements |

See official [gVisor](https://gvisor.dev/docs/), [Firecracker](https://firecracker-microvm.github.io/) and [Kata](https://katacontainers.io/) descriptions; pVisor support is in [executors](../guides/executors/index.md).

## Data and analysis {#results}

### Minimal reference environments {#reference-comparison}

| Runtime | First output P50 ms | Repair/tests P50 s | Codex loop P50 s | Seven-tool filesystem task P50 s |
|---|---:|---:|---:|---:|
| pVisor VM | 86.29 | 3.97 | 10.93 | 4.08 |
| Firecracker PCI | 73.74 | 2.25 | 7.83 | 2.37 |
| QEMU q35 | 218.12 | 1.98 | 7.69 | 1.82 |
| QEMU microvm | 88.10 | 1.85 | 7.67 | 1.77 |

The seven-tool filesystem task includes launch, tool execution and exit. pVisor uses the latest release measurement from 2026-10-05 (2 vCPU / 4 GiB); Firecracker/QEMU use reference measurements from 2026-10-04 (2 vCPU / 16 GiB), with 30 samples each. This workload differs from repair/tests and the Codex loop; the other three columns retain their own measurements. See [filesystem comparisons](filesystem.md) for individual operations and P95.

Startup is in the same range, while pVisor VM tools and complete Codex loops are slower. Claude passes on reference VMs but times out initializing on pVisor VM. Kernels, virtio-fs/ext4, guests and networks differ; the table does not isolate libkrun as the sole cause.

### Complete Ubuntu deployment {#full-ubuntu}

Image-free pVisor Ready is about **110 ms**; complete Ubuntu takes **5.64 s** on Firecracker, **5.43 s** on QEMU q35 and **7.67 s** on microvm. Repair/tests take **4.61, 8.51, 8.12 and 10.33 s**, respectively. Internal tools take pVisor **4.02 s**, reference Ubuntu VMs **2.30–2.81 s**. Disposable short tasks benefit from less boot waiting; persistent environments require more than startup data.

Results use pinned artifacts and have not all been rerun alongside current integrated filesystem artifacts. [Startup](startup.md) · [Complete tasks and compatibility](agent-tasks.md) · [Methodology](methodology.md)

# Image and memory optimization

Agent attempts and concurrent tasks often use similar tools, dependencies and initial state, while accessing only part of that content. pVisor uses demand loading, sharing and compression to reduce repeated preparation and resource residency across these environments.

## The Host manages content; the guest executes tasks {#resource-layers}

The Host retrieves, verifies and caches images, responding to guest reads through file interfaces. Host-side Runtime and storage components manage RAM bases, shared pages and compressed content in supported configurations. The guest retains the kernel, tool processes and private state needed to execute the task.

This division lets execution environments use common Host content services while keeping file and memory modifications independent. See [Shared working sets and demand loading](../design/daemon/shared-working-set.md).

## Demand loading, sharing and compression {#resource-mechanisms}

The three mechanisms address untouched content, repeated content and cold content that must be retained:

| Mechanism | How it reduces overhead | Relevance to Agent execution |
| --- | --- | --- |
| **Demand loading** | Reads or decodes image content and [checkpoint RAM](resource-reuse.md#execution-checkpoints) when accessed, reducing upfront preparation | Tool calls use only part of an environment; restoration or branching need not immediately read all saved state |
| **Sharing and deduplication** | Reuses immutable image content or shares identical pages on supported RAM paths, preserving private modifications on writes | Tasks and attempts can reuse similar environments and starting points with fewer duplicate copies |
| **Compression** | Stores compressible cold content in fewer bytes and decodes it when needed | Temporarily inactive state that must be retained may occupy less space even when it cannot be shared |

Image-content sharing and guest RAM sharing operate at different layers: using the same image does not establish shared RAM. Demand loading shifts some cost to first access, while compression requires CPU and recovery buffers. See [Shared image cache](../reference/shared-image-cache.md) for image loading and [Memory architecture and mechanisms](../design/memory-optimization/index.md) for supported memory paths.

## Coordination with lifecycle management {#lifecycle-and-cost}

[Sandbox lifecycle management](lifecycle.md) uses wait progress and guest state to determine **when to pause, offload and resume**. The mechanisms above determine **how content is loaded, reused and stored**. Offload requests reclamation of resident pages; whether content in backing storage is compressed depends on the configuration.

For example, tasks can reuse the same read-only image content while pausing or offloading according to their individual waits. Content reuse and lifecycle management combine according to supported configurations; they are not all enabled by default.

When evaluating concurrent capacity, count Host caches, shared pools, private writes and recovery buffers as well as guest RAM, and observe task duration and throughput together. A local compression ratio or lower memory usage for one instance does not directly translate into an additional VM count. See [VM memory benchmarks](../benchmarks/vm-memory/index.md) and [Concurrency density](../benchmarks/density.md).

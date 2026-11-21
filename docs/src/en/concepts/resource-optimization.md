# Image and memory optimization

A lazy image only needs to supply the content a tool reads to start. A tool that scans the whole image still has to perform those reads. Once tools are running, memory becomes a separate question: VMs that load the same library can share identical pages when page sharing is enabled. Distinct data that goes unused for long periods may be a better candidate for cold compression.

## The Host manages content; the guest executes tasks {#resource-layers}

The Host retrieves, verifies and caches images, responding to guest reads through file interfaces. Host-side Runtime and storage components manage RAM bases, shared pages and compressed content in supported configurations. The guest retains the kernel, tool processes and private state needed to execute the task.

This division lets execution environments use common Host content services while keeping file and memory modifications independent. See [Shared working sets and demand loading](../design/daemon/shared-working-set.md).

## Demand loading, sharing and compression {#resource-mechanisms}

The three mechanisms address untouched content, repeated content and cold content that must be retained:

| Mechanism | How it reduces overhead | Relevance to Agent execution |
| --- | --- | --- |
| **Demand loading** | Reads or decodes image content and [checkpoint RAM](resource-reuse.md#execution-checkpoints) when accessed, reducing upfront preparation | Tool calls use only part of an environment; restoration or branching need not immediately read all saved state |
| **Sharing and deduplication** | Reuses immutable image content or shares identical pages on supported RAM paths, preserving private modifications on writes | Tasks and executions can reuse similar environments and starting points with fewer duplicate copies |
| **Compression** | Stores compressible cold content in fewer bytes and decodes it when needed | Temporarily inactive state that must be retained may occupy less space even when it cannot be shared |

When two VMs hit the same [image cache](../reference/shared-image-cache.md), the host can reuse image content. That cache hit says nothing about whether their RAM is shared. With [page sharing](../design/memory-optimization/index.md) enabled, both VMs can read the same physical page while keeping writes private. A compressed block must instead be decoded and restored on access, which costs CPU time and needs a recovery buffer.

## Coordination with lifecycle management {#lifecycle-and-cost}

[Sandbox lifecycle management](lifecycle.md) uses wait progress and guest state to determine **when to pause, offload and resume**. The mechanisms above determine **how content is loaded, reused and stored**. Offload requests reclamation of resident pages; whether content in backing storage is compressed depends on the configuration.

For example, tasks can reuse the same read-only image content while pausing or offloading according to their individual waits. Content reuse and lifecycle management combine according to supported configurations; they are not all enabled by default.

When evaluating concurrent capacity, count Host caches, shared pools, private writes and recovery buffers as well as guest RAM, and observe task duration and throughput together. A local compression ratio or lower memory usage for one instance does not directly translate into an additional VM count. See [VM memory benchmarks](../benchmarks/vm-memory/index.md) and [Concurrency density](../benchmarks/density.md).

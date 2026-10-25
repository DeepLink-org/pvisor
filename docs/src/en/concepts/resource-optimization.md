# Image and memory optimization

Concurrent Agent environments often share dependencies, images, and parts of their state. pVisor reuses identical content in image loading and memory storage, and compresses eligible cold content to reduce preparation and runtime resource costs.

## Where optimization applies {#resource-layers}

Image caches reuse immutable file content. RAM bases and memory deduplication reduce repeated resident pages. Compression reduces the bytes occupied by compressible content. These mechanisms have different ownership, recovery paths, and prerequisites. Matching image names or file contents do not establish shared guest RAM; see [Shared working sets and demand loading](../design/daemon/shared-working-set.md).

## Demand loading, sharing, and compression {#resource-mechanisms}

**Demand loading** defers reading untouched image content or checkpoint RAM, reducing data that must be prepared at startup. Later reads may incur faults and loading latency. See [Shared image cache](../reference/shared-image-cache.md) for the current image format and configuration.

**Memory sharing and deduplication** reduce repeated pages across instances while preserving isolation through private writes. **Memory compression** reduces the bytes occupied by compressible content, including unique cold content, at the cost of encoding, decoding, and recovery headroom. See [Memory architecture and mechanisms](../design/memory-optimization/index.md) for platform requirements and combination limits. Select supported configurations rather than assuming all mechanisms can run together.

## Coordination with lifecycle management {#lifecycle-and-cost}

State reuse reduces repeated content. Lifecycle management determines when an environment runs and when it retains state while reclaiming resident resources. See [Sandbox lifecycle management](lifecycle.md) for pausing and whole-VM offload. Recovery and branching still need budgets for private changes, metadata, and temporary buffers.

Evaluate concurrent capacity using total node memory, recovery peaks, task duration, and throughput together. A local compression ratio or lower residency for one instance cannot be directly converted into an additional VM count. See the [VM memory benchmarks](../benchmarks/vm-memory/index.md) and [Concurrency density](../benchmarks/density.md) for available evidence.

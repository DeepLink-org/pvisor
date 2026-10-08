# Offload file format

The storage layout determines what can be read back, published and safely deleted.
A raw file favors simplicity; a compressed base/delta bundle trades that simplicity for reuse.

## Two storage models {#storage-models}

Raw backing stores RAM bytes at the VMM's compact file offsets, without a pVisor format header.
It is directly mappable, but logical length is not allocated disk space: the file can be sparse.
Neither a raw RAM file nor a compressed bundle includes the complete state of a recoverable VM.

In compressed mode the manifest identifies the sidecar directory and committed head.
Immutable generations hold a full base followed by deltas that inherit unchanged blocks.
Writable staging holds new raw pages; FUSE combines those pages with committed content
into the logical RAM file used by the live instance. Staging is not a historical snapshot.

## Random reads and integrity {#reads-and-integrity}

A block index resolves the latest source in the chain; a seek table locates its compressed frame.
Uniform blocks use fill entries. Reads validate metadata, ancestry, decoded length and block
checksums instead of decoding the entire RAM image. Integrity detects corruption, not a complete
VM checkpoint or an atomic capture of CPU, devices and external effects.

Current staging uses 4 KiB pages and compression uses 64 KiB blocks; host pages are separate.
Small units reduce partial-write and cold-read amplification but increase indexes and bookkeeping.
Large units can improve compression and reduce metadata, but a small access may decode a whole
block. [Local compression](compression-local.md) discusses the related memory/CPU tradeoff.

## Publication and retention {#publication-and-retention}

The writer flushes logical-file writes into staging, builds and synchronizes a new generation,
then publishes it without replacing an existing generation before appending a manifest head.
FUSE writeback or fsync alone does not publish a committed epoch; offload coordinates that boundary.
Immutability keeps published bytes stable, but it does not by itself keep their files alive.

GC currently retains the current head and ancestors plus explicitly pinned historical roots.
Old manifest head records are not retention roots, and standard offload does not automatically pin.
Reader leases are a lifecycle requirement for future concurrent inspection or portable bundles,
not a new lease facility promised by this format. Current inspection requires a stopped writer.
The separate [full environment snapshot](../environment-snapshot.md) store has its own restore leases.

## Compaction and capacity {#compaction}

Deltas reduce repeated storage, but deepen dependencies. Current depth-triggered compaction builds
a new full base while the VM is paused, decoding old blocks and encoding the entire logical RAM.
Budget for the old chain, new base under construction, staging and pinned history simultaneously.
Do not count the temporary file and its published name as two copies of the same new generation.
Small deltas do not predict compaction latency, peak space or cold-read decode amplification.

## Current limits and design direction {#status}

The manifest currently stores an absolute sidecar path; copying or aliasing it does not relocate
its dependencies. Open rejects a torn head-log tail rather than automatically falling back.
Staging validity masks live only in memory, and dirty kernel pages may not yet be staged;
normal exit does not commit post-resume writes. These files are not a crash-atomic full-VM archive.

Portable references, recovery-safe publication and coordinated reader/GC leases are future goals,
not capabilities established by these pages. See the [architecture](index.md) and
[RAM offload](offload.md) for scope; the existing [byte layout and schema appendix](../offload/disk-layout-and-schema.md)
owns binary fields, size formulas and parser limits rather than duplicating them here.

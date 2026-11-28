# aster-fuse

`aster-fuse` is the shared FUSE protocol crate for Rust native kernel.

It provides the common protocol vocabulary used by in-kernel filesystem clients
that talk to FUSE-compatible servers. Keeping these definitions in one crate
allows clients to share the same request and reply formats instead of maintaining
their own copies.

The pVisor `READINDEX` extension decodes bounded directory pages. The caller
owns lookup references from decoded backing records and releases unused
references with `BATCH_FORGET`. The extension requires pVisor device negotiation
because its FUSE INIT bit overlaps the standard io_uring capability.

Negotiating `PVISOR_DATA_OBJECT` extends only `OPEN` replies from 16 to 24
bytes. A qualified read-only open returns a nonzero object ID and a fixed
length; the ID identifies data within the session, not permission to open
it. `CREATE` and `OPENDIR` retain the base reply even on these sessions.

## Scope

This crate only describes the FUSE protocol. It does not choose a transport,
submit requests, manage queues, or own I/O buffers. Those responsibilities
belong to the filesystem client built on top of it.

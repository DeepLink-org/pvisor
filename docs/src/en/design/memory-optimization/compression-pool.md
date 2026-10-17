# Pooled-server compression

Share encoded cold content across authorized instances while keeping recovery
under each client's control. Pooling is worthwhile only when shared storage and
encoding work save more than coordination and service overhead cost.

## Target and current status {#status}

Linux has an experimental sealed compressed pool selected by
`pvisor-daemon serve --memory-pool --memory-pool-mode compressed`. The default
pool mode remains raw physical-page sharing. Compressed handoff, deduplication,
budgets, disconnected-reference lifetime and recovery after service exit have
process-level tests. A Linux x86_64/KVM two-VM correctness probe passed on
2026-10-18 with authorized kernel-fault userfaultfd access, covering guest
discard/refault, private writes and file I/O after pool termination.
There is no production-density or latency acceptance claim.

The macOS/HVF experimental heap pool remains separate: the service retains its
payload, and service loss can fail dependent VMs. No equivalent macOS sealed
backing is delivered.

## Ownership and data flow {#architecture}

The server indexes, encodes and budgets content; it receives no guest pointers
or authority over machine snapshots. Each instance independently owns complete
checkpoint publication through the [environment-snapshot contract](../environment-snapshot.md).

The VM captures stable 64 KiB cold blocks. The pool hashes decoded content and
compares complete bytes before reuse. Equal contents share one immutable encoded
memfd across sessions; unique compressible contents use fill or zstd encoding.
The client receives a read-only descriptor through SCM_RIGHTS and requires
write/grow/shrink/seal seals, bounded framing, matching identity and complete
byte equality. Only after this handoff and a second live-byte recheck may the
pager discard original RAM. `memfd` itself does not compress or deduplicate.

CPU or device access restores private writable RAM with local decoding,
checksum validation and `UFFD_COPY`. Restore and local release perform no IPC.
Release notifications flush in bounded batches on publication/stats; idle pager
maintenance also flushes them. Kernel-fault userfaultfd authority is required;
missing permission fails VM startup without fallback or global-policy changes.

After validated delivery, server exit disables new publication permanently but
retained handles remain decodable. Failure before delivery leaves original RAM
resident. Unexpected socket loss keeps publisher references and connection slots
budgeted until pidfd proves peer exit. A stale socket is never replaced, and
neither service reconnection nor host-crash recovery is implemented.

## Choices and tradeoffs {#tradeoffs}

Compared with [local compression](compression-local.md), the pool can avoid
duplicate payloads and redundant encoding across sessions. Sharing is restricted
to authorized trust domains; a content digest is not authorization. This is
encoded-object sharing, distinct from the default [physical page pool](deduplication.md#physical-pool).

The default ceilings are 512 MiB of page-rounded encoded backing, 32,768 objects,
32 connections and 32,768 references per connection. Encoded-byte accounting
includes frame headers and 4 KiB file-page rounding. An object must save at least
1/8 after rounding plus a conservative 256-byte metadata allowance. Rejected or
poorly compressed contents stay resident. Each object and delivered reference
also consumes a descriptor; OS descriptor limits can reject publication safely.
Indexes, threads, VM metadata and in-flight scratch add overhead outside the
payload limit. Reserve pool memory outside sandbox admission and retain decode
and restored-RAM headroom.

After transport failure, stats count local pinned bytes, objects and references;
cross-session sharing is unknown and reported as zero. A
`pvisor-sealed-pool publication_disabled=true accounting_scope=local-pins` log
marks that scope change. Connected stats report pool-wide backing and the
session's references. These counters do not measure physical-memory savings.

## Validation and constraints {#direction}

`just test pvisor-daemon` includes actual pool subprocesses, cross-session sharing,
service termination followed by local restore, stale-socket preservation and
pidfd-confirmed release after client exit. `just test pvisor` covers codecs,
immutable descriptor validation, content comparison, budgets and transport
ownership. These tests do not exercise guest eviction or KVM/device refault.

Validate guest reads, writes and device I/O with the sealed store before relying
on destructive reclamation. The existing Linux pager supplies two-phase
publication and recovery; encoding/IPC stay outside VM freeze windows. Recovery
can still queue behind an in-progress publication on the serialized store lock;
local decoding does not establish recovery scheduling or tail-latency acceptance.

A separate two-VM correctness driver waits for known shared and per-guest
payload blocks in the pool, terminates the service, then checks guest memory
and virtio-fs round trips. Use fresh short output paths and local firmware:

```bash
cargo build --locked -p pvisor-daemon --example compressed_pool_case
PVISOR_CASE_VM_LIBRARY_DIR=/absolute/firmware \
  PVISOR_EXPERIMENTAL_MEMORY_METRICS=1 \
  target/debug/examples/compressed_pool_case /tmp/pv-cmp-check
```

`result.json` records the outcome and per-guest recovery counters. Success
requires positive discarded bytes and increasing restored bytes after the
service-loss diagnostic, plus both guests completing their digest, private-write
and file checks. File writes precede full payload digest reads. The 2026-10-18
run passed after host authorization; the earlier permission failure remains a
separate failed attempt. The driver runs at most two 256 MiB/one-vCPU VMs and a
32 MiB pool. It does not validate the OpenSandbox API/bootstrap or measure
physical savings. The five permission-dependent Linux pager tests also passed,
including real KVM, kernel I/O and balloon reporting:

```bash
just test pvisor-vm -- -E 'test(cold_ram_linux::)' --run-ignored all --test-threads=2
```

Keep the cold-pager incompatibilities with snapshots, file/FUSE backing,
whole-VM [offload](offload.md) and KSM advice. Pool mode is fixed in persisted
configuration; drain old instances and use fresh state to change it.

## Evidence boundary {#evidence}

[Memory evidence](proof-of-concept.md#memory-evidence) establishes no known
production net gains or verified net physical-memory savings. Historical pool
measurements do not validate sealed backing. The [architecture index](index.md)
and [compression overview](compression.md) distinguish implementation, component
correctness, the scoped real-guest probe and remaining workload acceptance gates.

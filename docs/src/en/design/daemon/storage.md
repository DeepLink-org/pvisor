# Daemon storage and reclamation

Keep durable sandbox intentions in a bounded local registry. The native runtime owns private per-sandbox run state; prepared services own workload files and streamed outputs. These stores are not a centralized pVisor evidence repository.

## Private registry {#metadata}

```text
state directory (0700)
  daemon.lock       exclusive process ownership
  sandboxes.json    v2 header: version, owner UUID, empty sandboxes (0600)
  records/          private per-sandbox registry (0700)
    meta.json       version 2 and matching owner UUID (0600)
    sb-<uuid>.json  owner-wrapped sandbox record (0600)
    .record-*       uncommitted replacement temporary
  .records-*        migration staging directory
  .records-retired-* disposable pre-activation records directory
```

The store rejects symlink state/record directories and opens lock/registry files without following symlinks on Unix. Version 2 loads records only from the committed `records/` tree; header, metadata and each record must agree on the owner. Sandbox IDs, positive resources and endpoint credentials are validated. A missing header with an existing records tree, corrupt records or missing/mismatched metadata fail closed rather than falling back to an old snapshot.

The aggregate 16 MiB budget measures the logical version-1-equivalent snapshot: one owner/header plus sandbox keys and serialized record contents, excluding repeated per-file owner envelopes. It is not a physical directory-size cap; a valid v1 snapshot at the limit remains migratable. Individual JSON files are also bounded regular files.

Records retain image/argv, environment, metadata, CPU/memory reservation, creation/expiration, last observed status and sandbox endpoint token. Environment secrets are durable state: protect this directory and backups. They are not put in supervisor argv or host environment, but remain accessible to trusted host/runtime owners.

Runtime state additionally retains private `owner.json`, per-sandbox `preparing.json`/`identity.json`, `run.json` (generation/Run/Attempt IDs), lifecycle markers, owner lock, `control.sock`, `ports/` bridges and `run/` native storage, including `live-ram` backing and `tmp/` scratch (set as supervisor `TMPDIR`). A completed cleanup retains only its minimal tombstone/lock/marker fence. These support reconciliation/cleanup, not a public stage/checkpoint/artifact API.

Native runtime does not write `observation.json` caches or use them as liveness proof. Endpoint lookup authenticates the live supervisor and checks current Running state and deletion fences; full readiness checks remain at create, Inspect and resume. See [service access](lifecycle.md#endpoints).

## Commit barrier {#commit}

Each mutation clones/serializes only its target sandbox record, not the whole registry. Replacement writes and fsyncs a private unique `.record-*` file, atomically renames it to `records/sb-<uuid>.json`, then fsyncs `records/`. Removal unlinks that record and fsyncs the directory. Ordinary mutations rewrite neither `sandboxes.json` nor `meta.json`. The in-memory entry changes only after successful persistence.

A separate commit lock serializes admission decisions and durable mutations. The registry reader lock is released before disk I/O runs on the blocking pool, so readers can observe the last committed inventory while a write is in flight. Slow native VM operations and readiness checks still do not hold a global runtime mutex.

Rename/unlink may have committed before a later fsync error. The unchanged storage-failed latch refuses subsequent registry access instead of overwriting uncertain disk state from stale memory; preserve state, repair storage, restart and reconcile. Capacity and the logical 16 MiB limit bound retained records, not a million-task history. Incremental persistence is implemented, but performance has not been benchmarked. There is no append-only task journal, completion outbox or artifact CAS on this path.

## Version-1 migration {#migration}

A v1 `sandboxes.json` remains the authoritative full snapshot until activation. Only after the runtime factory accepts its persisted owner does Store initialize the v2 layout: write/fsync owner metadata and records into private `.records-*` staging, sync the staging directory, rename it to `records/` and sync the state directory, then atomically replace/fsync the root header with version 2 and an empty `sandboxes` map. This header replacement activates the committed records tree; native preflight follows initialization.

Interrupted staging or an unactivated `records/` copy cannot override the intact v1 snapshot. Reserved migration artifacts are validated for private ownership, names and regular-file types without requiring incomplete payloads to deserialize or following symlinks. Unactivated records can be renamed to `.records-retired-*` and reclaimed with staging remnants. Once the v2 header activates, records/metadata are mandatory; there is no v1 fallback. An uncertain activation fails the open, and the next open reads the surviving header.

NativeRuntime rejects occupied v1 state without `owner.json` before migration, and rejects any v2 header without that native marker even when its header map is empty. Preserve the original ownership state; do not erase records/reservations or fabricate a marker to bypass the guard.

## Removal and retention {#gc}

Persist deletion intent before native removal. Only confirmed native absence permits durable record removal and release of CPU/memory/count charges. Pending deletion, uncertain creation and missing-native Failed records are not opportunistically evicted to make room.

Cleanup validates durable owner/ID/generation and boot/cgroup bindings independently of launch resources: it does not require the original rootfs or firmware directory to remain present or guest argv/env/resource settings to pass launch validation. After proving native absence and acquiring the owner lock, it durably publishes `tombstone.json`, removes the empty owned cgroup and reclaims private run storage, live RAM backing, temporary overlays/specs, sockets and secret-bearing identity records and any legacy observation caches. A minimal directory retains the tombstone, owner lock and lifecycle markers to fence ID reuse. Interrupted reclamation resumes from the tombstone, including after cgroup removal; errors retain the reservation. Missing/replaced same-boot cgroups without this durable proof still do not authorize absence.

Deleting a sandbox does not undo remote effects or provide a retained evidence bundle. Externally provisioned image manifests/rootfs/firmware are not deleted by sandbox cleanup. The daemon API has no stage review/apply, artifact retention/download or external object-GC protocol. Do not remove native resources or registry state independently as recovery.

Native immutable cache/snapshot stores retain separate publication, integrity, pin and GC contracts; see [shared image storage](../shared-image-cache-storage.md) and [responsibility convergence](responsibility-convergence.md#authority). No daemon-to-node storage bridge is implemented.

Implementation: `daemon/store.rs` owns per-record publication, budget accounting and v1 activation; `daemon/mod.rs` owns reservation and deletion ordering.

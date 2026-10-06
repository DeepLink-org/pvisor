# Daemon storage and reclamation

Keep durable sandbox intentions in a bounded local registry. Podman owns container storage; prepared services own workload files and streamed outputs. These stores are not a centralized pVisor evidence repository.

## Private registry {#metadata}

```text
state directory (0700)
  daemon.lock       exclusive process ownership
  sandboxes.json    version, owner UUID, sandbox records (0600)
  .registry-*       temporary checkpoint publication
```

The store rejects a symlink state directory and opens lock/registry files without following symlinks on Unix. The registry must be a regular file of at most 16 MiB, use supported version 1, and contain valid owner/sandbox identities, positive resources and endpoint credentials.

Records retain image/argv, environment, metadata, CPU/memory reservation, creation/expiration, last observed status and sandbox endpoint token. Environment secrets are durable state: protect this directory and backups. They are not exposed in Podman argument values, but remain accessible to trusted host/runtime owners.

## Commit barrier {#commit}

Each mutation clones the current bounded registry, applies its change, and serializes a complete checkpoint. It creates a private unique temporary file, writes and syncs it, renames it to `sandboxes.json`, then syncs the parent directory. Only a successful save replaces the in-memory registry.

A rename may have committed before a later sync error. The daemon therefore latches storage failure and refuses further registry access instead of overwriting uncertain durable state from stale memory. Repair and restart determine the surviving checkpoint.

Registry mutations serialize across sandboxes, but slow Podman operations and readiness checks do not hold a global runtime mutex. Checkpointing costs scale with retained registry size; they have not been benchmarked. Capacity and the 16 MiB limit bound records, not a retained million-task history. There is no append-only task journal, completion outbox or artifact CAS on the daemon path.

## Removal and retention {#gc}

Persist deletion intent before native removal. Only confirmed native absence permits durable record removal and release of CPU/memory/count charges. Pending deletion, uncertain creation and missing-native Failed records are not opportunistically evicted to make room.

Deleting a sandbox does not undo remote side effects or establish a retained evidence bundle. Workload data and image reclamation follow Podman/prepared-service rules; daemon API has no stage review/apply, artifact retention/download or object-GC protocol. Do not remove native resources or registry contents independently as routine recovery.

Native immutable cache/snapshot stores retain separate publication, integrity, pin and GC contracts; see [shared image storage](../shared-image-cache-storage.md) and [responsibility convergence](responsibility-convergence.md#authority). No daemon-to-node storage bridge is implemented.

Implementation: `daemon/store.rs` owns checkpoint publication; `daemon/mod.rs` owns reservation and deletion ordering.

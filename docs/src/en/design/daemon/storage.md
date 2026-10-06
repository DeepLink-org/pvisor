# Daemon storage and reclamation

Keep durable sandbox intentions in a bounded local registry. The native runtime owns private per-sandbox run state; prepared services own workload files and streamed outputs. These stores are not a centralized pVisor evidence repository.

## Private registry {#metadata}

```text
state directory (0700)
  daemon.lock       exclusive process ownership
  sandboxes.json    version, owner UUID, sandbox records (0600)
  .registry-*       temporary checkpoint publication
```

The store rejects a symlink state directory and opens lock/registry files without following symlinks on Unix. The registry must be a regular file of at most 16 MiB, use supported version 1, and contain valid owner/sandbox identities, positive resources and endpoint credentials.

Records retain image/argv, environment, metadata, CPU/memory reservation, creation/expiration, last observed status and sandbox endpoint token. Environment secrets are durable state: protect this directory and backups. They are not put in supervisor argv or host environment, but remain accessible to trusted host/runtime owners.

Runtime state additionally retains private `owner.json`, per-sandbox `preparing.json`/`identity.json`, `run.json` (generation/Run/Attempt IDs), `observation.json`, lifecycle markers, owner lock, `control.sock`, `ports/` bridges and `run/` native storage, including `live-ram` backing and `tmp/` scratch (set as supervisor `TMPDIR`). A completed cleanup retains only its minimal tombstone/lock/marker fence. These support reconciliation/cleanup, not a public stage/checkpoint/artifact API.

## Commit barrier {#commit}

Each mutation clones the current bounded registry, applies its change, and serializes a complete checkpoint. It creates a private unique temporary file, writes and syncs it, renames it to `sandboxes.json`, then syncs the parent directory. Only a successful save replaces the in-memory registry.

A rename may have committed before a later sync error. The daemon therefore latches storage failure and refuses further registry access instead of overwriting uncertain durable state from stale memory. Repair and restart determine the surviving checkpoint.

Registry mutations serialize across sandboxes, but slow native VM operations and readiness checks do not hold a global runtime mutex. Checkpointing costs scale with retained registry size; they have not been benchmarked. Capacity and the 16 MiB limit bound records, not a retained million-task history. There is no append-only task journal, completion outbox or artifact CAS on the daemon path.

## Removal and retention {#gc}

Persist deletion intent before native removal. Only confirmed native absence permits durable record removal and release of CPU/memory/count charges. Pending deletion, uncertain creation and missing-native Failed records are not opportunistically evicted to make room.

Cleanup validates durable owner/ID/generation and boot/cgroup bindings independently of launch resources: it does not require the original rootfs or firmware directory to remain present or guest argv/env/resource settings to pass launch validation. After proving native absence and acquiring the owner lock, it durably publishes `tombstone.json`, removes the empty owned cgroup and reclaims private run storage, live RAM backing, temporary overlays/specs, sockets and secret-bearing identity/observation records. A minimal directory retains the tombstone, owner lock and lifecycle markers to fence ID reuse. Interrupted reclamation resumes from the tombstone, including after cgroup removal; errors retain the reservation. Missing/replaced same-boot cgroups without this durable proof still do not authorize absence.

Deleting a sandbox does not undo remote effects or provide a retained evidence bundle. Externally provisioned image manifests/rootfs/firmware are not deleted by sandbox cleanup. The daemon API has no stage review/apply, artifact retention/download or external object-GC protocol. Do not remove native resources or registry state independently as recovery.

Native immutable cache/snapshot stores retain separate publication, integrity, pin and GC contracts; see [shared image storage](../shared-image-cache-storage.md) and [responsibility convergence](responsibility-convergence.md#authority). No daemon-to-node storage bridge is implemented.

Implementation: `daemon/store.rs` owns checkpoint publication; `daemon/mod.rs` owns reservation and deletion ordering.

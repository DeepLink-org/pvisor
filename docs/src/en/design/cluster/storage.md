# Metadata, artifacts and reclamation

The Controller persists low-frequency metadata, Workers persist pending terminal delivery, and large objects travel as immutable content references. Execution leases, evidence retention and download protection have separate lifetimes; reclamation checks all their roots.

## Controller metadata log {#metadata}

The log preserves accepted tasks, DAGs, assignments, environments, control/cancel intents, forks, native results and terminal receipts. Pure `Renew` updates do not append, consume log quota or fsync; historical frames remain readable. This low-frequency log is not synchronously replicated heartbeat state.

Each frame is `<BLAKE3 checksum> <JSON transaction>\n`, with a version and changes. The default limit is 1 GiB, with at most 16 MiB per frame. The log opens with `0600` and `O_NOFOLLOW`, verifies a regular file and exclusive lock, and syncs the parent directory. Complete invalid frames reject replay; incomplete final debris is removed only after successful full replay.

HTTP requests pass through a capacity-256 Dispatcher to one writer thread. It collects already queued operations up to 64 operations, 4 MiB of written frames or 2 ms of operation processing, sharing one fsync. Thresholds are checked between operations; individual operations are not split. Idle requests incur no deliberate batch wait.

The Scheduler lock spans the group and durability barrier. Success, read and conflict responses wait for that barrier; external GC retirement and download-eligibility checks share the lock and cannot see speculative state. Synchronous Scheduler APIs and GC callbacks retain immediate commits. Read-only/renewal-only groups add no persistent frames and do not fsync.

Disconnects do not cancel queued operations. An uncertain write/fsync fails all group replies, poisons the journal and closes the Dispatcher. Subsequent access is refused; restart/replay determines surviving transactions. The writer cannot roll back memory and resume serving after an uncertain commit.

## Quota refusal versus uncertain I/O {#quota}

| Failure | Behavior |
| --- | --- |
| Definite log-quota refusal | Operation not published; poll/recover can retain renewals while deferring new assignments/controls and expiry commits |
| Expiry commit refused by quota | Preserve resources and roots instead of ending only in memory |
| Read-only monitoring at definite quota limit | Read the derived view without invoking expiry; expiry visibility is asynchronous |
| First terminal/intent commit at limit | Can still fail; Workers retain delivery evidence until metadata capacity is restored |
| Actual write/fsync uncertainty | Poison and return unavailable; inspect storage and restart |

There is no online log compaction or task-identity GC. Check physical disk space before raising quota; deleting artifacts does not shrink metadata. Dropping the log and querying active Workers cannot reconstruct unassigned intents, graphs or acknowledged terminal history.

## Worker state and outbox {#outbox}

`STATE/tasks/TASK-GENERATION` holds assignments, native execution records, control observations, Bundles, traces and sealed spools. The outbox is bound to Worker ID and Controller URL, supports at most 4096 pending records and limits each record to 16 MiB. It is a terminal-delivery ledger, not the Controller's complete desired state.

```text
native execution ended / mounts released / local spool durably sealed
  → persist terminal outbox
  → optional native-done (reduce reservations)
  → upload objects and manifest
  → complete (Controller verifies and persists)
  → verify ACK identity, persist durable receipt
  → delete pending entry
```

Timeouts, lost ACKs and Worker restart retry the same key. Receipts persist before pending deletion; fenced delivery and accepted delivery have separate terminal records. Acknowledged execution directories, spools and checkpoint receipts are not automatically removed. Worker-local GC remains future work.

## Controller artifact CAS {#artifacts}

The artifact directory is derived through `journal.with_extension("artifacts")`. Objects use BLAKE3 digest and byte length references. Small manifests reference chunks and can retain native Bundles, traces, private VM writable layers and optional checkpoint publications. Uploading Bundle JSON alone does not transfer files named by its local paths.

Uploads bind the complete key and establish durable lease pins. Bulk I/O occurs outside the scheduling lock, with eligibility rechecked around critical publication. Completion verifies manifest/object hashes and lengths, required files, export capability and native Run/Attempt identity, rather than trusting arbitrary reported paths.

The legacy payload cap defaults to 8 GiB. `ArtifactStorageLimits` adds persistent, online-adjustable limits for unique object bytes/counts. Accounting includes deduplicated content, concurrent publication reservations and orphan uploads. These limits do not provide tenant-specific storage isolation. The Controller CAS remains local single-authority storage.

The log retains a unique artifact authority bound to the store. Restore must preserve the matching log and store; an arbitrary empty log or another shard's log cannot reclaim that directory.

## Checkpoint storage and atomicity boundaries {#checkpoints}

The Worker's optional FS/S3 snapshot repository differs from the Controller artifact CAS. The former stores complete sealed execution objects for import; the latter stores lease-bound evidence and publication references. Immutable checkpoint manifests validate every reference. Uploads, publication references, Controller receipts and native restores do not form one cross-storage transaction.

Retries depend on immutability, integrity checks and the outbox. Cancellation or loss preserves native observations already made. A readable checkpoint does not establish target-node compatibility; see [checkpoint lifecycle](lifecycle.md#checkpoint).

## GC roots and plan application {#gc}

| Root | Protected objects and release condition |
| --- | --- |
| Durable upload pins for active assignments | Lease uploads; released when the identity formally ends |
| Retained terminal manifests | Manifest and transitive references; released by explicit evidence retirement |
| Active download protection | Download references; released explicitly or by expiry |
| Publication reservations/open-read protection | Concurrent publishing/reading objects; released as operations settle |
| Pending reconciliation or historical leases without pin protocol | Conservatively blocks destructive reclamation to avoid missing roots |

GC uses preview/plan → apply. Immutable plan IDs expire after five minutes; candidates record inode/device/length and other identity observations. Apply rechecks current roots and file identities so replacement or newly referenced objects are not deleted. After restart, in-memory plans must be regenerated.

Optional terminal evidence retirement persists retirement before removing roots and reclaiming unreferenced contents. Tasks retain metadata and native results; downloads report retirement. Shared objects can be deleted only after all roots are released.

Downloads have explicit protection leases, five minutes by default, renewable with a maximum one-hour lifetime. Clients should renew throughout multi-object downloads and release protection afterward; a manifest GET is not permanent retention.

Pending reconciliation blocks destructive plan/apply for the entire shard. This conservative policy prevents missing roots before ownership converges, but one permanently unreachable Worker can delay global reclamation. Refinement must establish complete roots before removing the gate.

Implementation resides in `journal.rs`, `server/dispatcher.rs`, `artifacts.rs`, `artifacts/{quota,gc}.rs` and Worker `bin/worker/{outbox,artifacts,checkpoints}.rs`.

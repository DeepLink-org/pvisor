# pvisor-overlay-core

FUSE-neutral overlay mechanics and file-operation service shared by host FUSE
and VM virtio-fs, plus changeset review, apply, conflict detection and recovery.
Adapters own protocol inode identifiers, handles and permission translation.

## Validation

```sh
just test pvisor-overlay-core
```

## Physical lower immutability (conservative first version)

`LayerMutability::{Mutable, Immutable}` is the shared contract. Build a layout
with `OverlayLayout::with_lower_mutability(Vec<LayerMutability>)`; entries match
`lowers()` in highest-to-lowest priority order. Empty means all mutable for old
callers; nonempty length mismatch is `InvalidInput`. `lower_mutability()` returns
the validated declarations. Immutable is a caller promise that contents,
metadata, namespace, physical ancestors, mount identity and all hardlink aliases
stay stable for the entire overlay lifetime. A read-only mount or content-addressed
pathname does not prove this. `frozen_baseline` only changes baseline observation
semantics and never infers this promise.

Only successful physical immutable-lower metadata/path/parent identity results
are cached, scoped to one Core owner and keyed by lower index and relative path.
Merged winners, negatives, errors, directory inventories, upper, whiteouts,
opaque probes, policy, hardlink authorization and read/preimage observations are
not cached. Every request still walks merged prefixes and higher mutable layers.
The mutex-protected cache retains at most 4096 entries, clears on capacity, and
bypasses keys whose root plus relative path exceed 4096 bytes or whose relative
path exceeds 256 components (bounding parent identity storage too). First misses
collect complete parent identities; this can cost more than an uncached shallow
stat. No kernel TTL, KEEP_CACHE or content caching policy changes are made.

For same-artifact/same-input A/B, keep the immutable declarations identical and
set `PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE=1` in the serving host process before
Core construction for the uncached control. Unset it for caching. This only
disables optimization, not the caller's contract. `PVISOR_FS_PROFILE=1` emits
`overlay-core` measurements: `immutable_lower_cache_hits`,
`immutable_lower_cache_misses`, `immutable_lower_cache_evictions` (in `units`),
plus existing `layer_parent_stats`, `layer_leaf_stats` and mount identity counters.
Misses include uncached absence/errors; mutable/disabled/bound-bypassed probes
are not cache misses. Profiles are diagnostic, not performance acceptance.
The [owned-lower host FUSE experiment](../../benchmark/pvisor/IMMUTABLE_LOWER_CACHE_REPORT.md)
records same-contract cache-off/on engineering measurements; it does not establish
OCI/VM performance or review-journal costs.

`tests/lower_immutability.rs` covers contract validation, external live changes,
mixed precedence, upper mutations, hardlink denials, capacity and disabled control.

## Copy-up and truncation

Fresh regular-file copy-up for `O_TRUNC` can omit the content copy only when the
source has one link. Baseline preimage capture and journal ordering precede
upper publication. Existing uppers and shared hardlink inodes keep the normal
path; adapters perform the actual open and truncation after inode/handle
rebinding. `OverlayCore` / `FilesystemService::copied_hard_link_metadata(rel)`
provides a checked, non-mutating query of a lower hardlink's surviving copied-up
upper metadata. The group belongs to the Core owner and survives adapter FORGET
or inode reclamation; no adapter inode/canonical path is proof of content identity.
The query follows tracked upper renames/removals, propagates policy/I/O errors,
and does not materialize aliases or alter generic `metadata()` resolution.
HOST FUSE uses it to bind late aliases and select current inode attrs after
recursive materialization, including when the canonical node path is still lower.
The fresh single-link O_TRUNC path avoids the copy, not baseline hashing.
Baseline capture and content copying are separate: any descriptor-bound fingerprint callback must
prove baseline/source identity and preserve first-observation publication
races, composed lowers, remote backing and hardlink ordering.

## Apply inventory and publication

Apply planning collects one request-local upper inventory containing relative
raw paths, whiteout flags and no-follow metadata. Change classification and
hardlink dependency closure share that inventory. No inventory survives target
publication or upper pruning. Target checks occur at publication time;
completion/recovery collects fresh remaining changes after pruning. Recovery
must not reuse stale metadata across mutation boundaries.

Entry publication exclusively creates a private random temporary directory;
reserved-looking host names are never cleanup authority. Interrupted copies can
leave these directories behind, and retries do not sweep them. Replacement
backups require a durable ownership receipt before reuse or cleanup. Legacy
backups without that receipt are retained and require manual inspection, not
automatic adoption. Terminal `overlay.json` publications are authoritative over
an older matching Run overlay identity/generation. While the apply ledger is
pending, runtime reads project the terminal state to block drop but retain the
original generation for recovery. Apply reconciles under the Job mutation/Run
lease and target lock, then publishes the new runtime fence only after ledger
commit. Recovery failures are errors, not `AlreadyApplied`. This does not make
`run.json` and the core ledger an atomic publication.

## Directory enumeration

Host FUSE uses the service's names/type directory candidates and loads
attributes on demand for READDIRPLUS. Protected directory views validate
children at snapshot creation to hide denied hardlink aliases. See the host
adapter README for cookie and inode lifetime details.

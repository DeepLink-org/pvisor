# pvisor-overlay-core

FUSE-neutral overlay mechanics and file-operation service shared by host FUSE
and VM virtio-fs, plus changeset review, apply, conflict detection and recovery.
Adapters own protocol inode identifiers, handles and permission translation.

Writable callers must exclusively own their upper/work/journal until teardown;
read-only observers do not acquire write ownership. Backing admission
canonicalizes upper/work once and validates their separation,
same-filesystem placement and lower/target isolation in Core. Reopening existing
backing validates work but does not enumerate or recover its temporary entries.
Read-only adapters open existing upper backing with no work or preimage journal;
they must reject view mutations themselves and do not promise fresh observations.

## Validation

```sh
just test pvisor-overlay-core
```

`resolve_checked` is the optional namespace-selection query; it preserves policy,
traversal and I/O errors and does not record an absence. The lossy `resolve`
shortcut has been removed. Namespace mutations must propagate failed probes
before publishing upper entries, rather than treating failures as missing names.
Remove/replacement checks reuse metadata from their checked resolution.

Ordinary rename between two names of the same regular-file hardlink is a no-op,
including a lower alias whose sibling has already copied up. NOREPLACE still
returns EEXIST, and a missing source returns ENOENT even when both names match.

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

New host/VM mount APIs default to Immutable and resolve that declaration before
Core construction. Low-level Core constructors and legacy saved empty lists
retain mutable semantics; runtime transparent rootfs/live workspaces explicitly
pass Mutable. No source is inferred immutable from a read-only path or apply policy.

By default, only successful physical immutable-lower metadata/path/parent identity
results are cached, scoped to one Core owner and keyed by lower index and relative
path. `PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE=1` additionally caches a checked
physical `MissingLeaf` for an eligible immutable lower, retaining its parent
identity vector. Unset or `0` keeps this experiment disabled; other values fail
with `InvalidInput` before upper/work/journal creation. Disabling the positive
immutable-lower cache also disables this extension. A negative hit continues to
later lowers; every request still checks upper, whiteouts, opaque markers, higher
mutable layers, policy and journal observations. It cannot hide a later mutable
or upper creation, and never grants authority to a future operation.
This lower cache never retains merged winners/absence, unavailable parents,
errors, directory inventories, upper, whiteouts, opaque probes, policy,
hardlink authorization or read/preimage observations. Every request still walks
merged prefixes and higher mutable layers.
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
Each eligible miss is also classified in `units` as
`immutable_lower_cache_misses_present`, `_missing_leaf`, `_unavailable_parent`
or `_error`. These partition misses without treating a missing physical parent
as a proven permanent absence. `immutable_lower_negative_cache_hits` counts
physical MissingLeaf hits separately; they are also included in the general
`immutable_lower_cache_hits`. Each Core emits an
`immutable_lower_negative_cache_enabled` measurement with units `0` or `1`,
including the zero value, describing its effective construction-time setting.
Positive and negative entries share all existing capacity/path bounds. The
experiment changes neither guest negative-dentry TTL nor the number of guest
LOOKUP requests. Creating a formerly missing name inside a declared immutable
physical lower violates the caller contract; discard that owner and construct a
new one for a new baseline rather than expecting this cache to observe it.
Each environment-enabled profile emits `pvisor-fs-profile-start` once at owner
construction with its PID, component and instance. Complete diagnostic coverage
requires final cumulative records for the entire registered set and all expected
component families. A missing instance is unknown work, never a zero request count.
Legacy logs without registration remain partial evidence. Disabled profiles and
explicit in-memory test profiles emit no registration.
The [owned-lower host FUSE experiment](../../benchmark/pvisor/IMMUTABLE_LOWER_CACHE_REPORT.md)
records same-contract cache-off/on engineering measurements; it does not establish
OCI/VM performance or review-journal costs.

`tests/lower_immutability.rs` covers contract validation, external live changes,
mixed precedence, upper mutations, hardlink denials, capacity and disabled control.

## Explicit exclusive-upper absence experiment

`metadata_for_backing_lookup_optional(rel)` on Core and `FilesystemService`
returns `Ok(Some(BackingResolution))` for the same checked positive resolution
as `metadata_for_backing_lookup`, or `Ok(None)` only after a successful merged
absence check and successful required absence-observation publication. Policy,
alias, traversal and journal errors surfaced by the existing resolver retain
their original order and errno, including exclusion/journal ENOENT. That resolver
already interprets a backend NotFound as physical-candidate absence: the original
method returns ENOENT while the new method may return None after observation.
EIO/permission backend errors remain errors. Thus None does not prove permanent
physical or remote absence; a caching adapter must separately exclude or own
backend projections. Neither result reserves a namespace or
authorizes a future operation; adapters own any additional caching contract.

`FilesystemService::validate_exclusive_upper_namespace()` reuses the canonical
upper/preimage separation check without enabling a cache or changing its
configuration. Equal, ancestor/descendant and symlink-alias overlap is rejected.
`backend::attachment_overlaps(canonical_root)` reads the attachment registry
without invoking backend metadata/materialization, including for missing paths.
Both ancestor and descendant attachments count; relative/parent-component input
is invalid. Its result is instantaneous, not an attachment lease: a caching
adapter must independently forbid new overlapping attachments for its entire
owner lifetime. `tests/checked_backing_lookup.rs` exercises the optional result,
successful observation, native/backend/journal errors, policy/exclusions,
read-only overlap query and validation without implicitly enabling Core caching.

`FilesystemService::configure_exclusive_upper_absence_cache(bool)` is a
once-before-requests preference, disabled by default. It does not expand the
ordinary Core/HOST upper-ownership contract. The admitting adapter must guarantee
that upper namespace and directory-permission mutations use one exclusive
operation lock, with all cached queries holding its matching shared lock. Upper
root/physical ancestor/mount identity must stay stable for that owner. External
writers, lazy upper projections and hidden shared-operation namespace mutations
are unsupported. Existing descriptor-bound regular-file I/O may overlap if it
cannot change namespace or directory traversal permissions. The VM adapter owns
its explicit exclusive-upper preference and platform permission checks; physical
upper caching does not require immutable lowers or guest cache negotiation.

After acquiring the exclusive lock, hold
`FilesystemService::upper_namespace_mutation()` until publication, failure
cleanup and cancellation finish, then drop this opaque guard **before unlocking**.
It takes no adapter lock and drains no I/O. Construction suspends and clears the
cache; drop clears again and resumes the configured preference, even after a
partial failure. Enabled-owner mutation guards must not nest. Disabled owners
accept a no-op guard. Snapshot capture/restore and teardown follow the same
exclusive protocol. Cache state is private to one owner and is never serialized.
`exclusive_upper_absence_cache_enabled()` reports the configured preference,
including during temporary mutation suspension.

Enabling canonicalizes both upper and preimage directory paths and rejects equal,
parent/child or symlink-alias overlap with `InvalidInput`: read observations may
create journal names under a shared operation lock. A failed initial validation
leaves caching disabled and unconfigured. Disabled configuration preserves old
layouts. Reconfiguration is `InvalidInput` and preserves the prior preference.

Only the physical upper probe in merged component resolution retains
`MissingLeaf` or `UnavailableParent`. Complete successful parent identities up
to the missing leaf/unavailable ancestor are collected without request-local
prefix shortening and restored on hits. Entry attributes, errors, merged winners
or absence, policy, alias authorization, whiteout/opaque probes and journal
decisions are never retained. A MissingLeaf hit still checks whiteout/opaque
markers and later lowers. Mutex-protected entries are bounded to 4096, clear on
capacity, and bypass root-plus-relative paths above 4096 bytes or relative depth
above 256 components. Epoch checks reject insertion across any mutation boundary.
Guest metadata TTLs and data caching are unchanged.
Cache hits copy parent identities only when the request asks for them. Misses
retain a second vector only when both cache publication and evidence output need it.

Profiles retain explicit `exclusive_upper_absence_cache_enabled` units `0`/`1`.
`exclusive_upper_absence_cache_hits` partitions into `_hits_missing_leaf` and
`_hits_unavailable_parent`; `exclusive_upper_absence_cache_misses` partitions into
`_misses_present`, `_misses_missing_leaf`, `_misses_unavailable_parent` and
`_misses_error`. Disabled, suspended and bounds-bypassed probes are not hits or
misses. `_evictions` counts capacity clears; `_invalidations` counts enabled-owner
mutation begin/end clears, one each even if the map is already empty. Counts are
software-query diagnostics, not elapsed time or disk I/O.

## Copy-up and truncation

The compact log acquires a nonblocking exclusive file lock once at open and
retains it until drop. Competing writable handles/processes, including hardlink
aliases, get `WouldBlock`. Open replays and validates the log, repairs only an
incomplete tail and persists the recovered prefix. The owner then uses its
in-memory index without per-operation file locks, binding checks or external
tail refresh. Callers must preserve backing bindings; apply/recovery take over
only after the workload releases its writer. A new owner replays consumption.
The on-disk format and legacy journal reader remain unchanged.

Read-only log observers take no writer lock and neither repair nor sync. They
read a complete prefix, which may be stale; incomplete final frames are ignored,
complete corrupt frames still fail. Sealing requires a complete log after writers
have stopped. Read observations are not a snapshot or durable completion proof.

A repeated compact-log read can use the owner's published membership to skip
pre-publication resolution. Every request still resolves the live merged path
and checks policy, hardlink aliases and symlinks. Both formats retain process-local
journal locking around missing-entry checks and publication, releasing it during
fingerprinting. Compact same-path captures share temporary admission; unrelated
paths proceed. The first publication wins and durable promotion syncs that winner.
Capture failure publishes nothing; append/sync failure poisons the handle until
reopen. Pending paths are released on success and failure; fingerprints are never
cached. `coalesced_fingerprints` counts skipped captures after same-Core publication.
Shared-Core concurrency and apply-conflict coverage remains for both formats.
Legacy durable-membership state is valid until that owner closes too: apply
consumption starts after owner teardown, and a new owner observes the next
generation. Concurrent requests still arbitrate first publication on one owner;
a non-durable losing read does not invalidate a durable winner's membership.

`FilesystemService::prepare_copy_ups(paths, truncate)` prepares durable baseline
observations and regular-file bytes without publishing upper entries or changing
hardlink groups. Physical hardlink aliases share one prepared copy but retain
per-path observations. `use_prepared_copy_ups` installs opaque receipts for one
serialized operation; its guard cancels unused copies on drop. Normal operations
retain their own policy checks, journal ordering and upper publication. The
adapter must serialize mutations throughout preparation, installation and drop;
metadata queries may continue. Receipts belong to one Core owner (foreign
installation is EINVAL, overlapping installation EBUSY), and changed mutable source
identity/size/mode/ownership/link count/mtime/ctime fails consumption with EAGAIN.
Remote read-only projections use their logical attributes for nlink/ctime:
projecting another native cache alias must not look like a content mutation.
Backing identity and content metadata still have to match. This checks observed
changes rather than locking external mutable lower writers.

Preparation resolves each candidate once, sharing metadata with the hardlink
query. An Immutable selected lower reuses that metadata and skips repeated source
stats before/after copying and at publication. Mutable sources retain fresh
validation. Receipts still check owner, selected source identity and truncation;
namespace selection, permission checks, baseline observations and apply conflicts
are unchanged. `copy_source_revalidations` counts mutable source rechecks; the
immutable path performs none, including with metadata caching disabled.

Preparations use exclusively created random private directories in work, or
upper when work is absent. A batch shares one directory descriptor across all
receipts, so large directory renames do not consume one FD per file. The held
descriptor prevents directory identity reuse during cleanup. Names are hidden
from merged enumeration; cleanup never
adopts unrelated reserved-looking paths. Cancellation/failure may retain journal
observations, and interruption can leave private directories for manual recovery.
Review/apply ignore reserved copy-up directories instead of interpreting them as
deletion whiteouts; ordinary file whiteouts keep their deletion semantics.
They do not adopt or sweep those leftovers.
The host adapter reserves cache pending before preparation and owns request
ordering; VM callers retain their ordinary copy-up path. `copy_up_prepare`
profiles prepared byte work, while `copy_up_bytes` counts both serving paths.
`tests/prepared_copy_up.rs` checks publication, cancellation, source changes,
owner isolation, hardlink reuse/deduplication, remote projection aliases,
truncation, baseline ordering and 300-file publication with a 48-FD limit.

Fresh regular-file copy-up for `O_TRUNC` can omit the content copy only when the
source has one link. Baseline preimage capture and journal ordering precede
upper publication. Existing uppers and shared hardlink inodes keep the normal
path; adapters perform the actual open and truncation after inode/handle
rebinding. `metadata_with_copied_hard_link(rel)` returns physical metadata and
optional surviving copied-up hardlink metadata from one checked resolution.
The group belongs to the Core owner and survives adapter FORGET
or inode reclamation; no adapter inode/canonical path is proof of content identity.
The query follows tracked upper renames/removals, propagates policy/I/O errors,
and does not materialize aliases or alter generic `metadata()` resolution.
HOST FUSE uses it to bind late aliases and select current inode attrs after
recursive materialization, including when the canonical node path is still lower.
Host LOOKUP, path-based
GETATTR and READDIRPLUS share this result within the request; mutations still
perform their own validation and publish aliases before returning upper attrs.
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

Existing upper parents are checked with one no-follow metadata query during
create/copy-up preparation;
non-directories and I/O errors are not treated as absence.
Missing parents reuse checked resolution metadata instead of resolving and
statting the same source again. Directory enumeration reuses layer metadata and
collects whiteouts during its upper scan; it retains no directory snapshot across
requests. Attribute-bearing child queries still resolve fresh backing. Tree
operations use names only; the unused bulk attribute API has been removed.

## Experimental parallel stage sealing

`PVISOR_STAGE_SYNC_WORKERS=2|4|8` in the supervisor opts into a bounded pool
for regular-file `sync_all` during `stage::seal`. Unset or `1` retains the
original serial traversal; other values fail explicitly. Journal validation and
persistence still finish first. All independent upper-file drains must succeed
and all workers must join before bottom-up directory drains, parent persistence
and atomic `sealed-v1` publication. Errors (including worker startup/panic) do
not publish a new seal. Explicit workload fsync and journal synchronization do
not use this pool. The caller still owns the stage lease and must stop writers.

The inventory holds O(files + directories) paths and inode identities; regular-file
descriptors and worker threads are bounded by the selected worker count. Directory
iteration additionally holds O(tree depth) directory descriptors. Hard links are deduplicated, symlink/FIFO/device targets are never synced, and reopened
entries must retain their inode identity and type. This is an opt-in experiment,
not a change to the persistence contract or the default. Measure fresh-stage,
same-binary serial/parallel trials without tracing; retain failures and verify
lower immutability, upper bytes and completion markers before claiming a gain.

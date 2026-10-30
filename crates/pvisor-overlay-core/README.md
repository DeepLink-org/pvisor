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
mise run test -- pvisor-overlay-core
```

`resolve_checked` is the optional namespace-selection query; it preserves policy,
traversal and I/O errors and does not record an absence. The lossy `resolve`
shortcut has been removed. Namespace mutations must propagate failed probes
before publishing upper entries, rather than treating failures as missing names.
Remove/replacement checks reuse metadata from their checked resolution.

Ordinary rename between two names of the same regular-file hardlink is a no-op,
including a lower alias whose sibling has already copied up. NOREPLACE still
returns EEXIST, and a missing source returns ENOENT even when both names match.

## Explicit private VM backing metadata (Linux)

`OverlayLayout::with_private_backing()` opts an exclusively owned fresh upper
into separate physical and logical metadata **before** Core imports lower-root
permissions. The adapter must enforce guest DAC using logical identity. Ordinary
Core/host FUSE and image constructors do not opt in. Native VM host-root layouts
(target `/`, single lower `/`) opt in both during mountless runtime preparation
and VM device construction; other layouts retain their defaults.

Core stores `user.pvisor.private_backing=1` on the upper root. Copied directories
remain host-owner-owned 0700 and regular files 0600, with their original logical
UID/GID/full Unix mode in `user.containers.override_stat`. No lower is chmodded
or chowned. Private imports require successful xattr publication, not best-effort
ownership loss. Failed parent imports remove their newly created empty directory;
construction failure may retain initialization metadata and journal observations.
Symlinks and special files retain the existing native ownership limitations.
The serving adapter must keep guest CREATE/MKDIR/CHMOD identities virtual too.

This first version rejects private root/directory/regular-file metadata imports
containing `system.posix_acl_access` or `system.posix_acl_default` with
`io::ErrorKind::Unsupported`. The complete source xattr-name list is checked
before changing destination permissions or installing any source attributes, and
that same checked list is used for the copy. Native POSIX ACL installation can
rewrite physical mode bits; silently dropping ACLs or chmodding after installing
them would instead alter logical ACL semantics. Neither is supported. Failed
copy-up/preparation leaves no published entry, and failed parent import removes
the newly created empty parent. Root ACL rejection leaves an existing upper's
permissions/attributes unchanged and publishes no private marker or identity;
initialization may still leave empty backing directories or journal observations.
Ordinary non-opt-in overlays retain their existing ACL-copy behavior.
`tests/private_backing.rs` installs real Linux binary POSIX ACL xattrs through
syscalls (no libacl/setfacl dependency) and covers both ACL names, file/directory
and prepared copy-up, parent cleanup, root rejection and native defaults.

Persisted private roots retain the flag and logical identity across repeated
initialization, even when upper contains no directory entries. Opening existing
backing performs no permission conversion. An invalid marker, missing private
root identity, foreign-owned private upper, or conversion of an unmarked nonempty
upper fails. Explicit existing-open opt-in requires a persisted marker; legacy
unmarked native VM snapshots are not automatically migrated. Private backing is
Linux-only; other platforms return Unsupported.

Shared helper contracts (exported at this unmigrated crate's existing root):

- `PRIVATE_BACKING_XATTR` and `OVERRIDE_STAT_XATTR` name the two controls.
- `logical_identity(path, &metadata) -> io::Result<backend::UnixIdentity>` uses
  backend attributes, then parsed override identity, then supplied native
  metadata. Permission-only override modes retain native type bits. Missing or
  unsupported attributes fall back; malformed values and other I/O errors fail.
  Callers retain no-follow path/metadata identity and synchronization ownership.
- `is_private_metadata_xattr(&OsStr)` recognizes exactly those controls. Logical
  fingerprints exclude both names from ordinary xattrs, but still compare the
  logical mode/UID/GID and all other supported user xattrs. Thus creating an
  equivalent virtual identity is not itself a conflict.

Review reports logical modes, including permission-only override modes. Apply
publishes logical UID/GID/mode through `copy_host_metadata` and strips both
controls from native target metadata (including pre-existing destination
controls); other supported user xattrs remain intact. Failure to remove a private
control is an error, not permission to leave it on the target. Native
symlink/special-file and unprivileged ownership limitations are unchanged.
Malformed identities and non-missing identity-read errors fail review/planning;
Prepared execution/recovery preflights the upper identities before restoring
backups or writing any target. This requires exclusive upper ownership, not a
promise of atomicity against concurrent upper modification.

**Raw snapshot copies instead retain physical metadata and both controls** so
reopening preserves backing access and identity. `copy_snapshot_metadata`,
`snapshot_overlay_upper` and `restore_overlay_upper` do not translate identities
or filter private controls; failure to publish a control is propagated. These
are distinct copy contracts, not a blanket replacement of native copying.
`tests/private_backing.rs` covers initialization/reopen, parent and regular-file
copy-up, lower isolation, explicit defaults, control filtering, corrupt state
and raw snapshot metadata. Apply's temporary-directory regressions cover logical
review/publication, control removal, full raw snapshot/restore and fail-before-write
behavior for corrupt identities in fresh apply and Prepared recovery.
VM mutation regressions cover guest modes, chmod, access denial and control-xattr
protection. Native KVM stage tests never apply to the real host root.

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
The [owned-lower host FUSE experiment](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/IMMUTABLE_LOWER_CACHE_REPORT.md)
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

Same-user apply processes use a common private lock directory beneath canonical
`/tmp`, independent of each process's TMPDIR. Target identity selects the lock;
directory/file ownership, modes, no-follow opens and unaliased files are checked
before acquiring it. A waiting apply rechecks conflicts after obtaining ownership.

Directory removal, rename/replacement, exchange and opaque materialization
validate physical descendants, including excluded names hidden from enumeration,
before publishing upper mutations. A hidden empty view is not authority to
delete the physical tree. A rejected parent operation leaves ordinary sibling
changes eligible for review/apply. Symlink targets are not traversed.

Apply planning and Prepared recovery independently check protected effects:
selected entries cannot affect excluded/denied paths, and whole-directory
replacement cannot contain reserved excluded paths. Denied descendants and
apply-selection exclusion globs are checked against the physical target tree,
including after restoring an interrupted replacement. Unsafe older stages fail
without silently dropping protection. This conservatively rejects replacement
of an ancestor of a reserved exclusion even when that excluded path is absent.

Ordinary lower metadata import filters the opaque control attributes recognized
by the merge algorithm. Other user xattrs are retained. Raw upper snapshots and
reopening an existing upper preserve legitimate control attributes.

macOS uses the same ASCII case-insensitive exclusion matching as the serving view.

Apply planning collects one request-local upper inventory containing relative
raw paths, whiteout flags and no-follow metadata. Change classification and
hardlink dependency closure share that inventory. Ancestor closure includes directories
that must be created or converted from a non-directory, not independent metadata
changes on directories that already exist. Select an existing directory explicitly
to apply its staged metadata; selecting only a child leaves that metadata staged.
Recorded existing-parent preimages remain conflict checks without becoming
publication dependencies. Child publication advances only their journaled mtime,
not mode/owner/xattrs; interrupted-child recovery tolerates that timestamp change
but still rejects parent permission/ownership/xattr changes. Opaque-directory and
hardlink dependency checks remain unchanged.
No inventory survives target publication or upper pruning. Target checks occur at publication time;
completion/recovery collects fresh remaining changes after pruning. Recovery
must not reuse stale metadata across mutation boundaries.

Entry publication exclusively creates a private random temporary directory;
reserved-looking host names are never cleanup authority. Interrupted copies can
leave these directories behind, and retries do not sweep them. Replacement
backups publish an initialized, synced ownership receipt by no-replace rename
before reuse or cleanup. Failed initialization cannot publish the apply ID;
abandoned private initialization siblings are not adopted or swept. Target locks
use the fixed `/tmp/pvisor-apply-locks-<effective-uid>` namespace across TMPDIR
settings and remain private, no-follow, single-link files. Legacy
backups without that receipt are retained and require manual inspection, not
automatic adoption. Terminal `overlay.json` publications are authoritative over
an older matching Run overlay identity/generation. While the apply ledger is
pending, runtime reads project the terminal state to block drop but retain the
original generation for recovery. Apply reconciles under the Job mutation/Run
lease and target lock, then publishes the new runtime fence only after ledger
commit. Recovery failures are errors, not `AlreadyApplied`. This does not make
`run.json` and the core ledger an atomic publication.

### Restricted filesystem-root apply

A canonical apply target of `/` supports ordinary file additions, modifications
and deletions without a sibling backup directory. The runtime/CLI must separately
require explicit `--rootfs`, effective UID 0 and root-controlled stage storage;
this low-level mechanism does not grant privileges or bypass ownership/locks.

Root-target planning rejects selected directory deletions, opaque/whole-directory
replacements, type changes involving directories and root-directory metadata
changes before publishing any new Prepared ledger or changing the target. Select
ordinary file paths separately to leave unsupported changes staged. In particular,
a staged `ROOT_METADATA_NAME` marker is not an implicit dependency of a selected
file: the file can apply while the marker and root metadata remain staged and the
target root's ownership/mode remain unchanged. Explicitly selecting `.` or using
`--all` still selects that root metadata and fails before ledger/target writes.
Creating new directories and explicitly updating non-root directory metadata
remain subject to the normal
preimage, dependency-closure, exclusion and access-policy checks. Opaque root
application remains unsupported.

Prepared and TargetApplied recovery rechecks these root restrictions before
restoring/replaying effects or pruning the upper. Root transactions do not look
up, create, adopt or clean sibling replacement backups; unsupported older records
remain pending for manual inspection. Ordinary directory targets retain their
existing backup ownership, recovery and cleanup behavior. System directories on
other filesystems are not made transactional by this restriction.

This is not an atomic system upgrade or a multi-file transaction against other
writers. Stop concurrent writers. Selecting package files independently of their
package metadata can leave the system/package database inconsistent. Tests use
pure canonical-path classification and the same private root policy against
temporary-directory targets, never apply to the real `/`.

### Read-only preflight and project-scoped root stages

`plan_overlay_apply` remains the selection/dependency planner, not a conflict
check. Use `apply::preview_overlay_apply(record, lowers, selection)` for actual
preflight. It shares execution preparation: stage durability/terminal/immutable
admission, dependency closure, exclusions/policy, logical upper identity and
recorded target preimage validation. A pending Prepared **or TargetApplied**
record fails rather than being recovered. Preview takes no target lock and writes
no target, journal, overlay metadata or ledger; it neither repairs nor consumes
observations. Stop stage writers first. A successful preview is an observation,
not a reservation: execution checks again under the target lock. Legacy ordinary
apply retains its existing explicit execution-time implicit recovery behavior.

The returned `ApplyPreview` separates `selected` (requested matches after
exclusions) from `required` (implicit dependency entries). Their union is the
execution plan. `candidates` is the scope's full change list before selection and
exclusion filtering, including changes this restricted apply mode may not support.
All paths, including scoped results, remain in original overlay/root coordinates;
runtime is responsible for mapping them for project-relative display and input.
Empty `paths` and `includes` means all eligible changes even with nonempty
`excludes`; excluded ancestors exclude their entire subtree.

Linux root-target stages can use `preview_overlay_apply_scoped` and
`apply_overlay_selected_scoped` with an explicit `ProjectApplyBoundary`:

- `target`: the absolute project directory, necessarily equal to `/` joined with
  `overlay_prefix`; this is not an arbitrary destination override.
- `overlay_prefix`: the nonempty relative project prefix in the original overlay.
- `device`/`inode`: the durably recorded project directory identity.

**Runtime must validate this boundary against its durable stage/project record**,
not manufacture it from an apply argument or a newly observed replacement root.
These APIs require the original overlay target `/`; they do not grant privileges,
use sudo, or relax ordinary system-root apply admission. Complete first-touch
preimages are mandatory for scoped apply (no legacy apply-time baseline fallback).
The first implementation admits ordinary file additions, modifications and
removals, plus newly created supporting directories. It conservatively rejects
symlinks, special files, directory deletion/type replacement, opaque effects and
existing-directory/project-root metadata publication. Selecting an ordinary child
does not implicitly select existing-parent metadata. Dependencies must stay
strictly below the prefix: hardlinks crossing scope are rejected, never expanded
outside the project. Selection exclusions and overlay policy still take priority.
Scoped apply, like preview, refuses pending work; explicit global recovery must
complete it before preparing a new batch.

Scoped publication acquires each temporary entry's sync/hardlink FD **before**
installing final logical metadata, and syncs that held FD without reopening a
possibly unreadable pathname. File xattrs are copied while the temporary file is
owner-readable/writable, then final mode is installed through the held FD. Newly
added directories use their desired metadata with temporary owner `rwx` access
until every selected child is durable. Final directory metadata is installed
bottom-up, with xattrs copied before restricting permissions; a logical `0555`
new directory with children is supported, as is ordinary `0755` publication.
Prepared recovery accepts either final or this narrowly defined working mode
**only for added directories**, restores their working access top-down, replays
children, then performs the same bottom-up finalization. Existing-parent
permission/ownership/xattr fences are not relaxed.

Non-root preflight conservatively returns Unsupported for files whose logical
mode lacks owner-read permission (including private logical `0200` files), and for
new directories that lack owner read/search access or whose logical owner is not
the caller. These checks run in both preview and execution **before Prepared**:
private physical `0600` storage is not evidence that a published write-only file
can later be read and hashed during crash recovery. Supporting such files requires
a further recovery protocol, not merely fixing the publication-time fsync.
Recovery of older unsupported Prepared intents fails before target/journal writes
and leaves the intent pending for inspection; it does not chmod unreadable target
files to bypass preimage validation. No root privilege is requested or simulated.

Target access opens the project path component-by-component with
`O_DIRECTORY|O_NOFOLLOW`, checks the recorded device/inode, and holds directory
FDs. Descendant traversal uses the same no-follow opens; target operations use
single-component names beneath held proc-fd parents. Publication rechecks root
and parent bindings, never recursively deletes a target directory, uses no-replace
publication for new directories, and pins in-scope hardlink source inodes instead
of following mutable target names. Symlink/replacement rebinding fails closed.
This remains a non-transactional filesystem apply: stop concurrent editors,
namespace movers and other writers. FD anchoring does not lock out a writer
moving an already-open directory or creating new hardlink aliases, nor does it
turn multi-file application into an atomic transaction against hostile same-user
processes. Other platforms return Unsupported for project-scoped apply.

The original root-coordinate upper, journal and overlay record are never rebased
or copied into a temporary subtree. Private ledger schema **3** atomically records
the boundary by apply ID alongside a schema-3 scoped `ApplyRecord`; the shared
`ApplyRecord` DTO has **no new fields**. Ordinary records remain schema 2. Ledger
state updates retain every boundary; a scoped record missing its boundary fails
closed. Older core readers reject schema 3 rather than interpreting it as root
write authority. `recover_pending_applies` uses each recorded boundary for
Prepared replay and TargetApplied completion, checks identity/protection before
mutation or pruning, and preserves original root-coordinate paths through journal
rebasing, consumption and partial multi-apply. Project batches never create or
clean system-root/sibling replacement backups. After TargetApplied, recovery only
finishes pruning/consumption/commit and does not replay target writes.

`apply.rs` temporary-directory root-mimic regressions cover zero-write previews
(success, conflict, dirty stage, Prepared and TargetApplied pending), requested
versus supporting new directories, exclude-only selection/excluded ancestors,
cross-scope and in-scope hardlinks, deletions/unsupported types, partial multi-apply,
partially published Prepared recovery, partially pruned TargetApplied recovery,
missing durable boundaries, and root/ancestor symlink or identity rebinding.
They never apply to the actual host `/`. Non-root permission regressions use
private logical `0200` files to check zero-write Unsupported preview/execution and
older-intent recovery admission, and logical `0444` files below new `0555`
directories to check fresh publication, working/final-directory Prepared recovery,
post-publication Prepared recovery, final modes, xattrs and control filtering.
These DAC-specific tests run as the real non-root caller (not under sudo); root
runs skip their bodies because root would mask the permission failures.

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

## Ephemeral stage completion

`stage::begin(journal, StageDurability::Ephemeral)` explicitly selects a fresh,
non-reusable stage. The policy marker is still durably published before exposing
writers, so a recovered stage cannot silently inherit the legacy strict policy.
Existing managed or legacy stages cannot be converted to this mode.

After all writers stop, `stage::complete` validates the complete observation log
and upper directory, then writes `completed-volatile-v1` without syncing the
journal/upper tree. It never writes `sealed-v1`. Interrupted or corrupt journals
still fail completion. Successful completion leaves workload output available
for immediate inspection; it does not promise its survival after a host crash.
`OverlayState::Staged` alone is not a durability assertion.

`stage::seal`, `require_sealed`, apply, checkpoint and stage reuse reject this
policy even if a seal file exists. A completed ephemeral stage admits no further
writes. Default checkpoint and strict stages still seal through `complete`.
Explicit workload fsync and `sync_journal` retain their existing behavior;
preimage capture/hashing, initialization, metadata/audit writes, copy-up, RAM
writeback and offload safety are not disabled. This option isolates the cost of
the automatic stage completion barrier; it is not a no-I/O or no-journaling mode.

### Unrecorded ephemeral stages

`stage::begin_with_recording(path, Ephemeral, StageRecording::Off)` admits a fresh
metadata directory. Durability and recording are bound by one atomically written
`durability-v1` marker (`pvisor.stage.ephemeral-unrecorded/1`). Existing policies
retain recording; incompatible settings, nonempty metadata, unknown markers and
mixed observation data are rejected. Keep this metadata path in host/VM configs:
it enforces the ephemeral lifetime and checkpoint/apply refusal even though
`entries`, `log-v2` and the observation-completeness marker are never created.

Core skips first-observation fingerprints, journal publication and the directory
walk used solely for descendant preimages. Access/path/alias checks, copy-up,
whiteouts, permissions, hardlinks and normal writes remain. Explicit filesystem
fsync still reaches the backing file/directory; only the absent observation
journal drain becomes a no-op. Workload output is retained for inspection, but
there is no first-observation history, apply/conflict detection, checkpoint or
cross-run reuse. Initialization/audit metadata remain durable. This does not
disable integrity hashes used by images or other unrelated subsystems.

`tests/unrecorded_stage.rs` covers retained file semantics and lower isolation,
zero observation fingerprints/publications, concurrent copy-up, access denials,
policy admission, corrupt/mixed state and read-only completion. Stage tests also
exercise apply/reuse refusal with both ephemeral recording settings.

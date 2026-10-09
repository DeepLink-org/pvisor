# pvisor-overlayfs

**Cross-platform FUSE overlay for pVisor staging (macFUSE / libfuse).**

Owns the unprivileged, in-process FUSE overlay: ordered multi-`lowerdir` merge,
directory upper, portable `.wh.*` whiteouts, and the optional
standalone `pvisor-overlayfs` diagnostic CLI.

Does not own review, apply, drop, or Run lifecycle.
[`pvisor`](../pvisor/README.md) links this crate as a
library, owns the FUSE request thread, and commits whiteouts through
`apply_overlay`. Portable, FUSE-neutral overlay mechanics live in
`pvisor-overlay-core::service::FilesystemService`, also used by VM virtio-fs
without a host FUSE mount. VM lazy images attach the shared remote read-only
backend directly; host lazy images retain a FUSE adapter over that backend.
Protocol inode/handle ownership and platform permission handling remain in
their entry adapters.

Whiteouts match pVisor's `apply_overlay`, so review → apply works the same
across host FUSE and virtio-fs.

Linux-only container features are out of scope: UID/GID namespace mapping,
`metacopy`, `redirect_dir`, SELinux labeling, and capability semantics are not
emulated.

## Public API boundary

The only public module is `pvisor_overlayfs::api`. It contains declarations and
contracts only: no method bodies, default trait implementations or conditional
API shapes. All public methods are declared in API traits and implemented by
private adapters; do not add public inherent methods or expose FUSE state.
Low-level tests stay inside the crate. Linux and macOS share the same signatures;
backend selection and platform checks remain private.

| Trait | Implementing type | Responsibility |
| --- | --- | --- |
| `OverlayConfiguration` | `OverlayMountConfig` | Construct owned mount inputs without I/O |
| `OverlayMounting` | `OverlayFs` | Background/foreground mount and mountpoint probe |
| `OverlaySessionControl` | `OverlaySession` | Query and consume a background mount owner |
| `FilesystemMetrics` | `FsMetrics` | Snapshot a shared, bounded-path observation sink |

```rust
use pvisor_overlayfs::api::{OverlayConfiguration, OverlayMountConfig};

let config = OverlayMountConfig::new(
    vec!["/workspace".into()],
    "/stage/upper".into(),
    Some("/stage/work".into()),
    "/stage/merged".into(),
);
```

Import `OverlayMounting` to use `OverlayFs::mount`, `run_foreground` or
`is_mountpoint`; import `OverlaySessionControl` for session methods and
`FilesystemMetrics` for `snapshot`. Configuration uses shared Core/overlay-core
DTOs.

Mount preparation is not transactional: errors may leave directories or journal
initialization behind. Explicit unmount consumes ownership even on failure;
drop attempts cleanup but discards errors. A finished request thread does not
prove mount detachment. The macOS mount-table probe fails closed, while Linux's
metadata heuristic can miss same-device bind mounts. Metrics clones share a
thread-safe sink; snapshots do not freeze the filesystem, retain at most 8192
paths, and do not represent all host filesystem activity. See `src/api.rs` for
field-level security, platform and ownership contracts.

`tests/api_contract.rs` checks the syntax boundary and portable public behavior
without mounting FUSE.

## Lower stability and cache experiments

Set `api::OverlayMountConfig::lower_mutability` using the shared
`pvisor_overlay_core::LayerMutability`, in `lower_dirs` order. Construction leaves
it empty (all mutable); mounting rejects a nonempty length mismatch before path
preparation, after the existing macOS backend installation probe. The caller
must keep physical backing contents, metadata, namespace, ancestors and mount
identities stable until session teardown; `read_only` is not such proof. Upper
and the merged mount must never be declared immutable. Private mount preparation
passes declarations into `OverlayLayout::with_lower_mutability`.

The shared Core caches only successful immutable physical-lower metadata and
parent identities, not merged results or policy/observations. Upper and mutable
precedence checks stay fresh. This Core cache does not itself enable the explicit
kernel policy described below. For the older Core-cache A/B use
identical declarations with `PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE=1` set before
starting the host request process versus unset. `PVISOR_FS_PROFILE=1` exposes
Core hit/miss/eviction `units`; see the overlay-core README for bounds and exact
counter names. These are diagnostic knobs, not new product CLI options.

## Explicit Linux HOST owned-view kernel cache

`Metadata` supports writable views with explicitly immutable physical lowers and
caller-owned exclusive upper/work. `MetadataAndData` remains read-only: writable
KEEP_CACHE is explicitly rejected, not downgraded. No writeback caching is enabled.
If lifetime ownership or backing stability cannot be established, use the legacy
`Disabled` default. `Uncached` is a separate zero-TTL comparison control.

The platform-independent API lives exclusively in `api.rs`:

```rust
use pvisor_overlayfs::api::{
    KernelCacheConfig, KernelCachePolicy, OwnedViewContract,
    OverlayConfiguration, OverlayMountConfig, ReadObservationSemantics,
};
use pvisor_overlay_core::LayerMutability;
use std::time::Duration;

let mut config = OverlayMountConfig::new(
    vec!["/stable/lower".into()], "/owned/upper".into(),
    Some("/owned/work".into()), "/owned/merged".into(),
);
config.read_only = false; // Metadata permits adapter-mediated mutations
config.lower_mutability = vec![LayerMutability::Immutable];
config.kernel_cache = KernelCacheConfig {
    policy: KernelCachePolicy::Metadata,
    ttl: Duration::from_secs(60),
    owned_view: Some(OwnedViewContract {
        exclusive_upper_and_work: true,
        fixed_metadata_and_aliases: true,
    }),
    read_observation: ReadObservationSemantics::StableView,
};
config.validate_kernel_cache()?;
# Ok::<(), anyhow::Error>(())
```

For same-binary/same-input comparisons change only `kernel_cache.policy`:

| Policy | Entry/attr TTL | Negative TTL | Regular-file open |
| --- | --- | --- | --- |
| `Disabled` (default) | legacy 1 second | zero | no KEEP_CACHE |
| `Uncached` | zero | zero | no KEEP_CACHE |
| `Metadata` | configured TTL | configured TTL | no KEEP_CACHE |
| `MetadataAndData` | configured TTL | configured TTL | KEEP_CACHE |

Default behavior is unchanged: Disabled retains the original one-second positive
TTL and ENOENT replies without negative caching. Uncached is the explicit
metadata-cache-off control, **not DIRECT_IO**: normal kernel page caching inside
an open handle is retained. Neither control requires an ownership contract or
changes another crate's default. No mode enables writeback caching.
The default configured TTL is 60 seconds; enabled TTLs must be positive and at
most 60 seconds, never infinite. MetadataAndData only sets KEEP_CACHE on regular
files in the admitted read-only stable view; no mutable/copy-up mapping qualifies.
Existing lower open-handle and copy-up rebinding semantics are unchanged on the
disabled writable path; cache admission does not change those mechanisms.

Enabled policies require all physical lowers explicitly `Immutable` in exact
order, both ownership assertions, explicit `StableView` observation semantics,
`default_permissions`, owner-only FUSE access and the Linux HOST backend.
Only MetadataAndData additionally requires `read_only`.
They reject preimage journals, compact journal initialization, metrics sinks,
all custom path policies (including allow/deny/ask/warn and bound contexts), and
exclusions before preparation I/O. Cached accesses cannot satisfy callback audits
or first-content-observation promises; these are never silently skipped.
A read-only view, advisory lock or image digest is not an ownership proof.

The caller guarantees that all physical lowers' contents, namespace, xattrs,
permissions, ownership, **backing atime**, ancestors, mount identities and hardlink
aliases remain stable until actual detachment. Upper/work may change only through
this adapter; reads must not cause unobserved backing-atime changes. Arrange
noatime or equivalent backing behavior; the merged mount's NoAtime flag alone
does not stop backing atime updates. Upper/work must not be shared with another
session, apply, checkpoint restore or any out-of-band writer. The adapter obtains
nonblocking exclusive `flock` locks on canonical upper/work directory objects
(no replaceable lockfile), ordered by path, and retains the fds on the filesystem
owner until its teardown. Contention is an explicit error. These locks coordinate
only cooperating opted-in sessions, including across processes; disabled sessions
and noncooperating host writers are not covered. Writable cached views must not
have bind/namespace copies, overmounts or replacement mounts: failure detachment
must cover their only mounted view. Keep the mount namespace, server credentials,
helper availability and detach permissions stable throughout the session. The
caller must supervise **all** users and stop them if the server exits or OS
termination fails. Server abort alone is not containment of another process's
warm cache. Do not opt in if this failure-containment obligation cannot be met.
A stopped loop does not prove mount detachment or permit the caller to release
its backing-lifetime guarantee.

### Mutation effects, reply ordering and failure

Private `cache.rs` maintains bounded reply and entry-notification queues (256
batches each), plus a stop worker with a bounded queue (513 slots). A global
reservation bound allows at most 512 mutation batches across all workers/queues;
once stopping begins, no new mutation is admitted. The stop queue accommodates
those reserved tasks plus one coalesced control wakeup, never unbounded fallback
work. Each effects plan contains at most 4096 inode/entry keys plus one overflow
sentinel; subtree/alias collection is streamed. Exceeding a bound stops the session,
including when discovered after a partially completed mutation.
No FUSE callback waits for a worker, detaches the mount or calls a kernel
notification. Effects are captured before and after each attempted mutation,
including failures that may already have copied up or partly modified upper.
The planner records exact known object inodes, hardlink aliases, parent/ancestor
attributes and namespace entry keys. Rename/exchange/removal include the known
subtree and replaced objects, with both old and new entry keys. Parent attributes
are not expanded into unrelated sibling-entry evictions. Copy-up/recursive rename
rebind new physical object keys to existing FUSE inode identities. A lower
hardlink alias first discovered after copy-up is materialized against the current
upper object before lookup/readdirplus publishes attributes; it cannot publish
old lower size/mode under the copied-up inode. This uses Core's lifetime-owned
hardlink-group query, not adapter by_object or a canonical node path: it survives
final FORGET/reclaim and handles a canonical alias still in lower after recursive
materialization. Current attrs and read-open agree on the copied-up object.
This can materialize an upper alias
during lookup, and the resulting parent/object effects are observed as mutations.

Covered callbacks are create/mknod/mkdir/symlink, unlink/rmdir (whiteouts),
rename/exchange (including directory materialization and opaque effects), link,
writable open/copy-up/O_TRUNC, write, setattr, set/removexattr, fallocate and
copy_file_range. This observer is independent of the rejected read metrics sink;
mutation effects are never discarded merely because `StableView` is selected.

Ordering is:

1. Before modifying backing state, increment pending. Subsequent metadata/entry
   replies use **zero TTL**, including mutation replies.
2. The independent reply worker performs metadata-only
   `Notifier::inval_inode(ino, -1, 0)` for every affected inode, then sends the
   mutation reply. Negative offsets do not invalidate data or trigger writeback.
3. Linux's cooperative FUSE mutation handling updates its direct dentry/page-cache
   state. The separate entry worker then sends `inval_entry(parent, name)` for
   extra alias/namespace effects, followed by a final attribute expiry pass.
4. Only after completion is pending decremented; long TTLs resume when all batches
   finish. Entry invalidations may wait on a related kernel namespace lock, but
   cannot hold up the replies needed to release it. The reply worker never waits
   for the entry worker, including when that queue is full.

There is intentionally no blanket data invalidation on a writable inode: it could
wait on dirty pages / related writes. Metadata uses no KEEP_CACHE; content updates
use Linux cooperative write/truncate/fallocate/copy-file-range page-cache handling
and the adapter's existing shared-inode/copy-up fd semantics. The copy-file-range
loop acknowledges every written byte and returns the partial count on a later
read/write/zero-progress error, rather than returning an error that leaves modified
pages unreported to Linux. WRITE uses each request's current open flags, so
F_SETFL clearing/re-enabling O_APPEND is honored, including Linux append-pwrite.
COPY_FILE_RANGE is positional and clears stale backing O_APPEND before copying:
Linux admits that opcode only when the mounted target is non-append, but it does
not pass current open flags to the adapter. No preceding WRITE is required.
Extra entry
notifications are asynchronous, not a claim that each one precedes syscall
completion. The immediate checks in the real-mount tests have no notification
fence, retry or sleep. See the libfuse lowlevel notification deadlock contracts:
<https://libfuse.github.io/doxygen/fuse__lowlevel_8h.html>.

A writable cached mount must identify its own connection through mountinfo and
open `/sys/fs/fuse/connections/<id>/abort` before starting requests. Admission
first mounts a disposable FUSE session at the actual mountpoint and successfully
exercises verified detach→abort under the current credentials/namespace/helper
route. No filesystem user may start before mount() returns. Inaccessible fusectl
or a failed termination probe is an explicit mount failure, never a long-TTL
fallback. This is a current capability check, not proof against later OS or
credential/helper changes; the caller's lifetime/containment promises still apply.
Notification
errors (except fuser's harmless ENOENT), worker panic and queue overflow fail the
session: first lazily detach its mounted view via Linux umount or either
fusermount3/fusermount, verify mountinfo no longer contains it, then abort the
connection. **No abort endpoint write occurs if detach failed.** **Abort alone is insufficient**: real testing showed
warm metadata can still be returned by the kernel after connection abort.
Detach precedes abort waking a failed syscall; failed replies wait for completed
termination. Reply-queue overflow is delegated to the stop worker, never handled
synchronously inside a FUSE callback. Rejected admission replies can return EIO
without mutation; EIO is not a general detach fence. Helpers have a shared 3-second
deadline with bounded kill/reap cleanup and no output pipes. Termination and its
waiters have a 5-second deadline; worker joins have bounded stop/escalation phases,
and writable session shutdown has a 15-second outer deadline. If termination
fails or exceeds its deadline, the hosting process terminates. **This fatal path
is not a metadata fail-closed guarantee for other processes:** failed detach can
leave their warm cache usable even after server death. The caller must enforce
its supervised-user stop policy, release descriptors and retain backing paths
for recovery. Successful detach also cannot revoke already-held descriptors.
`unmount()` reports the recorded notification error; upper mutations may already
have occurred and are not rolled back. Shutdown still aborts/detaches on transport
failure, but does not misreport normal post-detachment ENODEV as a new fault.

`StableView` is **not** first-content-observation equivalence, review/journal
compatibility, a snapshot or an audit of each read. Journals/custom policies/
exclusions/metrics remain explicitly rejected; no read logging is silently skipped.

`validate_kernel_cache()` is portable and side-effect free; mount entry points
perform it before macFUSE installation probing or directory/journal preparation.
macOS is rejected even with `backend=kernel`, since no notification capability
has been validated there. Production notification workers and the termination
probe compile only on Linux; portable worker tests also run on macOS.
**No VM mode exists**: virtio-fs has NotifyOpcode
declarations but no equivalent verified output notification transport; this
host-only DTO is not propagated through runtime/VM configuration. Runtime source
classification is not changed, and no environment variable enables this policy.
The older Core immutable-cache control remains separate.

Validation:

```sh
just test pvisor-overlayfs
cargo nextest run --locked -p pvisor-overlayfs --run-ignored ignored-only -E 'test(host_kernel_cache)'
cargo check --locked -p pvisor -p pvisor-cli
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p pvisor-overlayfs --no-deps
```

Real mount tests cover all four read-only policies, Uncached/Metadata writable
negative→create, copy-up and existing lower fd rebinding, cached reads plus write/
truncate/chmod/xattrs, hardlink inode/nlink aliases, rename replacement with a held
victim fd, directory rename with a held child fd, whiteout/opaque recreation,
second-session coordination, and immediate pending-worker teardown. A parallel
stress test runs four workers with 64 create/link/truncate/chmod/rename/delete
iterations each. Separate transport injection exercises notification failure on a
real warmed mount, detachment, preserved partial upper effects and error reporting.
Portable tests cover admission/boundary defaults and planner precision; worker
unit tests cover blocked entries, pre/post-reply failure, both queue saturations,
global reservation/effects bounds, failed-detach-before-abort ordering, helper
cleanup and process termination on a stuck-control deadline. Real mount tests
also cover previously unseen lower aliases after source-close, a real kernel
lookup/read against deterministically final-reclaimed adapter state, dynamic append
flags, positional copy immediately after clearing append (without an intervening
WRITE), and warmed cached pages after partial-copy fault injection (including
mid-buffer failures). Unit regressions explicitly remove all adapter object maps
and test canonical lower aliases after recursive materialization.

Tests use isolated temporary paths and no cache-enabling environment toggles.
Writable tests re-execute themselves through `unshare` into a private user/mount
namespace, then create a fresh **noatime tmpfs** fixture. They require unshare,
mount, real /dev/fuse and access to the connection's fusectl abort file. Neither
host mount options nor source classification is changed. Missing permissions are
failures in explicit mount runs, not silently skipped or weakened guarantees.
Read-only fixtures also verify backing-atime stability.

## Develop

### Prerequisites

macOS: install [macFUSE](https://macfuse.github.io/) (`brew install --cask macfuse`),
and enable its FSKit file system extension in System Settings → General →
Login Items & Extensions. The default backend is `fskit`; no kernel extension
or reduced boot security is needed. Use macFUSE 5.4.0 or later; older FSKit versions can corrupt small writes. The patched
`fuser` loads libfuse at runtime and uses channel callbacks, since FSKit does
not expose a device file descriptor. Standalone mounts must use a path under
`/Volumes`; pVisor chooses a unique mountpoint automatically.
FSKit requests do not provide caller credentials. The default overlay therefore
uses an owner-only root directory (`0700`) and OS permission checks;
`default_permissions` is required and `allow_root` is rejected for this backend.


Linux: FUSE3 development packages, for example `libfuse3-dev`.

### Build and test

```bash
just build release
just test pvisor-overlayfs
```

pVisor embeds the overlay library; it does not discover or launch an overlay
binary. Library consumers use `default-features = false` to exclude the diagnostic
CLI's argument parser and logger. The `cli` feature is enabled for standalone
builds by default; the executable requires it and is intended for diagnostics or
manual mounts:

```bash
cargo zigbuild -p pvisor-overlayfs --release --target x86_64-unknown-linux-musl
# → target/x86_64-unknown-linux-musl/release/pvisor-overlayfs (Linux)
```

## Directory enumeration and snapshot lifetime

`opendir` snapshots merged names and types, including whiteout/opaque and denied
hardlink filtering. Plain `readdir` uses those stable entries and index-based
cookies without eagerly loading child attributes or allocating new inodes.
Unknown inode numbers are returned as zero until lookup or `readdirplus` needs
an object-aware inode. Backends returning an unknown directory-entry type still
require metadata to determine its type; protected views retain their security
metadata checks.

`readdirplus` lazily loads fresh attributes for each visited entry on every plus
request; a handle pins names/cookies, not attribute freshness. Entries that
disappear before any attribute request are skipped without changing the
names/cookies snapshot. Repeated plus requests therefore cannot re-publish old
size, permissions or replacement-object attributes with a renewed kernel TTL. Known and
lazily assigned inodes remain pinned by the directory handle, and only delivered
plus entries acquire lookup references. Releasing the handle permits normal
inode reclamation. Deferred resolution is anchored to the handle-owning directory
inode's current path, so rename/exchange follows that directory rather than a
recreated old pathname. A replaced/detached directory handle is not rebound to
the replacement. Each child attribute request binds fresh metadata to the
current pathname/object inode and transfers the snapshot pin if the child was
replaced. An error reply discards the whole buffered page without acquiring
lookup references for any of its children; a successful page retains references
only for entries accepted into its buffer.

Copy-up and apply contracts are documented in
[`pvisor-overlay-core`](../pvisor-overlay-core/README.md).

## Links

- [Isolation architecture](../../docs/src/zh/design/isolation.md)
- [Review and apply effects](../../docs/src/zh/guides/review-apply.md)
- [`pvisor`](../pvisor/README.md)

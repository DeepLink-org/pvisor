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

`readdirplus` lazily loads and caches attributes for each visited entry. Entries
that disappear before their first attribute request are skipped without changing
the names/cookies snapshot. Attributes reflect the first plus request, not the
original `opendir`; subsequent plus requests use the cached value. Known and
lazily assigned inodes remain pinned by the directory handle, and only delivered
plus entries acquire lookup references. Releasing the handle permits normal
inode reclamation. Deferred resolution is anchored to the handle-owning directory
inode's current path, so rename/exchange follows that directory rather than a
recreated old pathname. A replaced/detached directory handle is not rebound to
the replacement. Each first child attribute request binds fresh metadata to the
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

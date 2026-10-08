# 0007: Share file services between host FUSE and virtio-fs {#adr-0007}

**Status: implementation backfill.** Records current source responsibilities without new formal approval or performance claims.

## Context {#context}

Host staging serves files through host FUSE; VMs request files through guest virtio-fs. Layer lookup, policy, copy-up, preimages and lazy-lower reads need identical semantics. Inodes, handles, credentials and queue completion depend on each protocol entry.

## Alternatives and choice {#decision}

Options include duplicating file semantics in both adapters, routing VM requests through host FUSE, or calling a shared service directly. The implementation chooses the third: `pvisor-overlay-core::service::FilesystemService` owns OverlayCore and is called directly by host FUSE and VM virtio-fs. It unifies local-file and immutable-image backend reads.

The service accepts file operations, paths and backing identities rather than `fuser::Reply*`, virtqueue descriptors or guest RAM pointers. Adapters retain protocol inodes/handles, permission translation, operation guards and completion publication. VMs require no intermediate `/dev/fuse` mount.

## Consequences {#consequences}

File semantics have one implementation, while the two entries retain platform and concurrency differences. virtio-fs workers hold RAM leases until used-ring publication, and freezing drains in-flight I/O. Shared-service content reads should not hold whole protocol handle-table locks. Lazy-lower copy-up or full export can still materialize unread content; interface reuse does not establish end-to-end speedups.

## Implementation and scope {#implementation}

Source entries are `crates/pvisor-overlay-core/src/service.rs`, `crates/pvisor-overlayfs/src/fs.rs`, `crates/pvisor-vm/src/devices/virtio/fs/`, and `crates/pvisor/src/image/cache/backend.rs`, `direct.rs`, `lazy.rs`. `pvisor-overlay-core` has not yet migrated to a sole `api` entry. Shared file services and API-visibility migration remain separate concerns.

[Filesystem subsystem](../overlayfs.md#filesystem-service) owns request paths, layouts and existing measurement scope; [VM subsystem](../vm-runtime.md#virtio) owns mapping lifetimes.

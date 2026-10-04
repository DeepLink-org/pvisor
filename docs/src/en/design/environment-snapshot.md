# Stage and execution snapshot CLI

On Linux x86_64 (KVM) and macOS Apple Silicon, `pvisor snapshot` saves and restores a complete VM environment. Snapshots use standalone instances: CPU, RAM, devices and the complete writable stage are saved during a single freeze; imported immutable rootfs bases are referenced and shared. After publication, the original runner exits; restoring creates a new working copy and continues the original processes.

## Usage

Prepare an unpacked Linux rootfs directory. Start the workload in terminal A:

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs -- bash
```

The defaults are 2 vCPUs and 256 MiB; use `--cpus` and `--memory` to configure them. `--ram-storage raw` is the default full RAM file baseline; `--ram-storage compressed` splits RAM into 64 KiB blocks, compresses them and deduplicates them in snapshot storage. Put command arguments after `--`; spaces within arguments are preserved. The input rootfs is imported into an owned base generation once; the workload writes through an OverlayCore stage and cannot modify the input or managed lower. Firmware uses pVisor's existing verified download and cache mechanism.

Save from terminal B:

```sh
pvisor snapshot save task-a
```

The command prints the snapshot ID. After successful publication, the runner in terminal A exits. If the requesting client disconnects after publication, the snapshot remains saved and the original instance still exits; use `list` to find the object.

Restore the original state:

```sh
pvisor snapshot restore SNAPSHOT_ID --name task-b
```

Restoring continues the original guest, processes and open files. Instance names contain 1–48 ASCII letters, digits, underscores or hyphens. Names are single-use; restore with a new name.

```sh
pvisor snapshot list
pvisor snapshot delete SNAPSHOT_ID
pvisor snapshot gc
```

Deletion refuses objects with active reader references. Once a running instance has installed all state and obtained a private working copy, it no longer depends on the published object; deletion does not damage that instance. `gc` removes unpublished writes and deletion staging left by crashes. It does not delete published objects or clean up active writers.

All commands accept `--store /path/to/store`; different terminals must use the same directory. The default is `pvisor/snapshots` under the system user data directory, typically `~/Library/Application Support/pvisor/snapshots` on macOS. The control socket lives in a private directory with a short path, owned by the same UID, to avoid macOS Unix socket path length limits.

### Eager RAM restore

```sh
pvisor snapshot restore SNAPSHOT_ID --name task-eager --eager-ram
```

Default restore uses the existing lazy FUSE RAM path. `--eager-ram` verifies
raw RAM's full digest, or decodes/verifies compressed RAM into a private unlinked
file before mapping it with `MAP_PRIVATE`. This mode needs no RAM FUSE mount;
startup performs upfront RAM reads/materialization and makes no lazy-startup
claim. The stage/base contract and saved RAM encoding stay the same.

The macOS test has reproduced an exiting-task/ownership-pipe stall with the
lazy FUSE RAM path, including exit after a successful second terminal save;
its root cause is not yet established. Eager restore provides a validated
alternative for stage snapshots while this lifecycle issue remains open.

## Base generations and stage layout

Reuse a base without importing the full tree for each run:

```sh
BASE_ID=$(pvisor snapshot import-base --rootfs /path/to/linux-rootfs)
pvisor snapshot run --name task-a --base "$BASE_ID" -- bash
pvisor snapshot verify-base "$BASE_ID"
```

`--rootfs` performs the same import before starting a new instance; `--base`
reuses a generation from the selected store. Specify exactly one. Import is an
independent copy with full content/native metadata validation, and its cost is
outside save/fork. IDs identify local owned generations, not OCI image digests
or cross-host portable layers. The original directory can subsequently change
or be removed. A missing/replaced base fails explicitly.

The stage container is `STORE/runs/NAME/rootfs/{upper,work,preimages}`. All three
are captured, including whiteouts, opaque state, hardlink/copy-up origins and
review observations. Base trees live in `STORE/bases/BASE_ID/rootfs`; save and
fork do not copy them. New raw stage snapshots use manifest v4; compressed stage
snapshots use v5. Legacy v1–v3 full-tree objects keep their original restore
path and compatibility checks. Unknown formats fail closed.

Saved objects pin bases with durable hardlinks; each running VM retains an
independent file-lock lease. Deleting a saved object cannot invalidate its
running forks. `gc` also evicts base generations with no saved references or
runtime leases, so an ID returned by `import-base` is not a permanent retention
request. Working stage directories are not automatically deleted.

This private cache relies on exclusive ownership: attaching it as an overlay
lower prevents guest writes, but does not protect against a hostile host or
same-UID processes editing cache files. Do not manually modify bases. Warm
opens check the seal and root generation identity without hashing every base
file; normal virtio-fs restore verifies captured inode identities/content.
`verify-base` performs the full integrity audit, including unvisited files.
Copy-up records hardlink source paths so new snapshots do not scan whole bases
to rediscover forgotten inode origins; old snapshots retain a compatibility
fallback.

## Persistent compression and forks

Select persistent compression at startup:

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs \
  --ram-storage compressed -- bash
pvisor snapshot save task-a
```

The compressed path reuses the cold-page pool's content identities and Fill / Zstd / Raw encodings. Legacy full-environment manifest v2 records RAM block order, lengths and the overall digest. Content lives in `STORE/content/ID`, with hard links inside objects retaining durable references. Repeated blocks within a snapshot and identical blocks across snapshots share encoded objects; publication does not depend on a live pool process. Content and references are durably synced before publication, and restore is permitted only after the entire environment directory is atomically published. New raw RAM snapshots use manifest v3, recording a SHA-256 digest for every 64 KiB block; a compatibility path still reads format v1.

`delete` removes environment references. Running VMs retain content needed for later reads through an open raw RAM file or active staging hard links. `gc` only reclaims content without snapshot or active staging references, and cleans staging references left by exited processes. Restore checks the manifest, machine state, filesystem tree and RAM index before startup, then verifies content digests when reading RAM blocks. Read failures stop restore or the VM; corrupt content must never be replaced with zero-filled RAM. A content digest does not authenticate content from untrusted users.

One object can restore into multiple independent VMs. `fork` is a visible alias for `restore`, **forking from a saved object**. Save the source instance first, then run these commands in separate terminals:

```sh
pvisor snapshot fork SNAPSHOT_ID --name branch-a
pvisor snapshot fork SNAPSHOT_ID --name branch-b
```

Both VMs continue from the same saved point. Guest boot IDs/PIDs retain their saved values, while host instance names, runners and working directories differ. Each instance obtains private RAM mappings and an independent stage while sharing immutable bases; heap changes, file changes and open handles are independent. Once installation completes, neither depends on the published object. This is not a live fork that keeps the source VM running, and it does not provide a shared live RAM fork or incremental RAM capture. Stage file copies use clone/reflink where available, with verified independent inode semantics and a data-copy fallback.

Stage copies retain ACLs, xattrs, permissions, timestamps and full content
validation. macOS uses `COPYFILE_CLONE` where supported and reapplies metadata
to preserve setuid/setgid bits; Linux tries `FICLONE`. Unsupported/cross-filesystem
cloning falls back to data copies, while permission, space and I/O errors abort
publication. This reduces physical copying without removing stage traversal,
hashing or durable synchronization, and makes no fixed latency promise.

Restore loads RAM on demand through a read-only FUSE RAM file and `MAP_PRIVATE` mappings. The first guest or device access reads and verifies the corresponding block, decoding compressed blocks on demand. Guest writes use private COW pages without changing the snapshot or other forks. Startup no longer decodes all RAM, writes a temporary RAM file or copies all RAM. Legacy v1 raw snapshots still check the whole digest before mapping on demand. Linux requires usable `/dev/fuse` and mount permissions; macOS requires the macFUSE kernel backend. Each runner owns a RAM mount and a bounded block cache; decoded page caches are not yet shared across runners. A separate exit watcher detaches RAM after the runner closes its ownership pipe, using force detach on macOS and lazy detach on Linux. This covers exits that bypass destructors; however, macOS tests have observed exiting tasks stranded before the pipe closes, both after abrupt termination and a drained terminal save. The root cause remains open, and lazy RAM exit cleanup is not yet a reliability guarantee. Ordinary RAM offload cannot discard restored COW pages; saving a new full snapshot still reads all RAM. Only stage trees are copied for the new profile; legacy full-tree objects still materialize their complete trees. Direct saving of an active cold-page pager is not yet connected, and restore latency and physical memory savings require measurement.

## Initial limits

- `snapshot save` saves instances started by `snapshot run` or `snapshot restore`. Ordinary `pvisor run` Jobs, DAX, Gateway and cold-page pools are not yet connected to this save entry point.
- The current version requires the same host, the same host boot, and identical CLI build and firmware. After a CLI upgrade, old snapshots refuse restore because their build identities differ.
- Input is a complete directory; `image=...` image resolution is not available. Networking, additional host mounts, active external connections, writeback, unlink-open, nested VMs and cold-page pagers are unsupported. The first version provides no network device or host network passthrough.
- Hard links outside the tree, sockets, FIFOs, device files and nonzero BSD file flags are explicitly refused. The source directory should be exclusively controlled by the user, without external host writers.
- Saving still captures all RAM and copies the complete stage; compressed mode only changes RAM's persistent encoding and sharing. RAM restore is lazy, but this does not promise incremental capture, a fixed VM fork latency or physical density gains.
- VM stages remain in `STORE/runs/NAME/rootfs/{upper,work,preimages}`. `gc` only handles snapshot staging and does not automatically delete working copies. After stopping an instance, you can archive or clean up its directory.

Native Linux root directories containing their own `/init.krun` can use `snapshot run --native-init` without specifying a workload command.

## Validation

`scripts/check-snapshot-cli.py --ram-storage compressed --fork` uses the product binary and ordinary guest launcher to validate run/save/fork/list/delete/gc: original arguments, boot ID/PID, background threads, a 32 MiB heap and open files remain continuous. The input and original private directory are deleted after saving; the published object is deleted after restoring, and the original task still completes validation. Two concurrent branches independently modify their heaps and open files; changes in the first branch do not affect the other. Both keep running after environment deletion and GC while their live RAM/base dependencies remain pinned. Each restored branch is then saved again and stopped while frozen; after deleting those final checkpoints, GC must reclaim all unreferenced RAM/base dependencies and mount cleanup must finish. `--ram-storage raw --fork` validates the raw-format baseline; add `--eager-ram` to either encoding to validate the alternative with complete exit/dependency cleanup. This experiment validates correctness, not performance.

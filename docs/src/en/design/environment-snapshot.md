# Full environment snapshot CLI

On Linux x86_64 (KVM) and macOS Apple Silicon, `pvisor snapshot` saves and restores a complete VM environment. Snapshots use standalone instances: CPU, RAM, devices and complete file copies are saved during a single freeze. After publication, the original runner exits; restoring creates a new working copy and continues the original processes.

## Usage

Prepare an unpacked Linux rootfs directory. Start the workload in terminal A:

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs -- bash
```

The defaults are 2 vCPUs and 256 MiB; use `--cpus` and `--memory` to configure them. `--ram-storage raw` is the default full RAM file baseline; `--ram-storage compressed` splits RAM into 64 KiB blocks, compresses them and deduplicates them in snapshot storage. Put command arguments after `--`; spaces within arguments are preserved. The filesystem is first copied into a private instance directory, so the workload does not modify the input rootfs. Firmware uses pVisor's existing verified download and cache mechanism.

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

## Persistent compression and forks

Select persistent compression at startup:

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs \
  --ram-storage compressed -- bash
pvisor snapshot save task-a
```

The compressed path reuses the cold-page pool's content identities and Fill / Zstd / Raw encodings. Full environment manifest v2 records RAM block order, lengths and the overall digest. Content lives in `STORE/content/ID`, with hard links inside objects retaining durable references. Repeated blocks within a snapshot and identical blocks across snapshots share encoded objects; publication does not depend on a live pool process. Content and references are durably synced before publication, and restore is permitted only after the entire environment directory is atomically published. New raw RAM snapshots use manifest v3, recording a SHA-256 digest for every 64 KiB block; a compatibility path still reads format v1.

`delete` removes environment references. Running VMs retain content needed for later reads through an open raw RAM file or active staging hard links. `gc` only reclaims content without snapshot or active staging references, and cleans staging references left by exited processes. Restore checks the manifest, machine state, filesystem tree and RAM index before startup, then verifies content digests when reading RAM blocks. Read failures stop restore or the VM; corrupt content must never be replaced with zero-filled RAM. A content digest does not authenticate content from untrusted users.

One object can restore into multiple independent VMs. `fork` is a visible alias for `restore`, **forking from a saved object**. Save the source instance first, then run these commands in separate terminals:

```sh
pvisor snapshot fork SNAPSHOT_ID --name branch-a
pvisor snapshot fork SNAPSHOT_ID --name branch-b
```

Both VMs continue from the same saved point. Guest boot IDs/PIDs retain their saved values, while host instance names, runners and working directories differ. Each instance obtains complete private RAM and file copies; heap changes, file changes and open handles are independent. Once installation completes, neither depends on the published object. This is not a live fork that keeps the source VM running, and it does not provide inexpensive shared runtime RAM or file COW.

Restore loads RAM on demand through a read-only FUSE RAM file and `MAP_PRIVATE` mappings. The first guest or device access reads and verifies the corresponding block, decoding compressed blocks on demand. Guest writes use private COW pages without changing the snapshot or other forks. Startup no longer decodes all RAM, writes a temporary RAM file or copies all RAM. Legacy v1 raw snapshots still check the whole digest before mapping on demand. Linux requires usable `/dev/fuse` and mount permissions; macOS requires the macFUSE kernel backend. Each runner owns a RAM mount and a bounded block cache; decoded page caches are not yet shared across runners. A separate exit watcher unmounts RAM after the runner exits, including exits that bypass destructors and `SIGKILL`. Ordinary RAM offload cannot discard restored COW pages; saving a new full snapshot still reads all RAM. File trees remain complete copies. Direct saving of an active cold-page pager is not yet connected, and restore latency and physical memory savings require measurement.

## Initial limits

- `snapshot save` saves instances started by `snapshot run` or `snapshot restore`. Ordinary `pvisor run` Jobs, Overlay/DAX, Gateway and cold-page pools are not yet connected to this save entry point.
- The current version requires the same host, the same host boot, and identical CLI build and firmware. After a CLI upgrade, old snapshots refuse restore because their build identities differ.
- Input is a complete directory; `image=...` image resolution is not available. Networking, additional host mounts, active external connections, writeback, unlink-open, nested VMs and cold-page pagers are unsupported. The first version provides no network device or host network passthrough.
- Hard links outside the tree, sockets, FIFOs, device files and nonzero BSD file flags are explicitly refused. The source directory should be exclusively controlled by the user, without external host writers.
- Saving still captures all RAM and copies the complete file tree; compressed mode only changes RAM's persistent encoding and sharing. It does not promise incremental capture, lazy loading, inexpensive VM forks or physical density gains.
- VM working copies remain in `STORE/runs/NAME/rootfs`. `gc` only handles snapshot staging and does not automatically delete working copies. After stopping an instance, you can archive or clean up its directory.

Native Linux root directories containing their own `/init.krun` can use `snapshot run --native-init` without specifying a workload command.

## Validation

`scripts/check-snapshot-cli.py --ram-storage compressed --fork` uses the product binary and ordinary guest launcher to validate run/save/fork/list/delete/gc: original arguments, boot ID/PID, background threads, a 32 MiB heap and open files remain continuous. The input and original private directory are deleted after saving; the published object is deleted after restoring, and the original task still completes validation. Two concurrent branches independently modify their heaps and open files; changes in the first branch do not affect the other. Both keep running after environment deletion and persistent block reclamation. `--ram-storage raw --fork` validates the raw-format baseline. This experiment validates correctness, not performance.

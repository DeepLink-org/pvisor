# A stopped guest submits one packed tree

[B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng), engineering experiment, not a gate cell. Can a producer hand off one bounded object instead of exporting and publishing each file through virtio-fs, **including** the cost of turning the object back into ordinary host files?

All arms use the [same probe](staged_tree.c), UID 1000, eight files of 512 KiB each, and 4,096 writes of 1 KiB. The controls implement [#212's directory publication](../evidence/linux-fw-file-io.md#explicit-publication-eight-files-as-one-new-directory). The local arms use a private producer-owned directory on a verified guest tmpfs; they do not require the tmpfs root to be world-writable.

| Arm | During generation | Who builds and publishes the ordinary output tree? |
|---|---|---|
| `shared` | Small writes to shared `.pending/part-i` | Guest syncs each file and `.pending`, renames to `ready`, syncs the parent. |
| `local` | Small writes to guest tmpfs | Guest copies into eight shared files in 16 bulk writes, then completes the same sync/rename protocol. |
| `pack` | Identical guest tmpfs files | Guest makes 16 bulk writes to **one** precreated shared file, syncs it and exits. Host imports into an unshared `.pending`, syncs each file and `.pending`, renames once with host `RENAME_NOREPLACE`, syncs the parent. |

`pack` moves metadata operations and publication to the host; it does not eliminate the 4 MiB transfer or eight file syncs. It **adds** a host read and write of the payload and a temporary 4 MiB file. The guest sees submission, not acceptance. Compare `pack` to `local` to isolate the new handoff, not just to `shared`. Compare namespace and bytes, not #212's historical timings or POSIX metadata identity.

## Transaction boundary

- The [single-operation driver](packed_tree_once.rs) creates a fresh private trial and opens a regular input inode **before** boot. The importer runs the trusted binary outside the VM root only after zero VM exit and the exact `PACK_SUBMITTED` marker. It reads the retained descriptor, not a path supplied by the guest. No socket, privileged service, filesystem capability or runtime API is added.
- The input is exactly 4 MiB, split into eight equal parts in order, with no encoded names, lengths, permissions, links or executable commands. Import uses one 256 KiB copy buffer. Control files are created as guest UID/GID 1000:1000 through virtio-fs; packed files are created by the **host importer**, with its host credentials, directory group inheritance and umask, requesting mode `0600`. On this test host all three happened to be 1000:1000 mode `0600`; preserving guest ownership, xattrs, times or modes is not the contract.
- Wrong input length/type, short I/O, sync/close failures and name conflicts are errors. Length is a framing check, not content authentication or a guest disk quota: a wrong but full-length payload can be published. The independent benchmark oracle would reject that operation **after** publication.
- Destination and transfer are separate: only the transfer file is shared with the guest. A partial transfer can be observed there, never at the final `ready`. The host destination is caller-owned with no concurrent writer; its temporary name is not secret from other host processes with access. Competing final names are not replaced. The guest control arms assume exclusive publication: the Rust virtio-fs implementation [uses ordinary FUSE rename after its guest namespace checks](../../pvisor-kernel/kernel/core/src/fs/fs_impls/virtiofs/inode/mod.rs#L523-L550), not a host no-replace guarantee against racing host writers.
- Kill the VM even after it syncs the pack but before successful exit: no import. Kill the importer before rename: `ready` is absent, possibly leaving `.pending`. After rename: the complete tree is visible; until the output parent sync succeeds its persistence is **unconfirmed**. An error then does not roll the name back. Successful sync covers entries under the caller's parent on a filesystem honoring `fsync`; it does not sync ancestors of a newly provisioned trial or prove power-loss recovery. No automatic retry, cleanup or durable receipt.
- This is a new flat tree only. It does not merge existing paths, rebind open handles, preserve arbitrary metadata, implement `:stage` review/apply, or change any ordinary `:rw` acknowledgement.

## Reproduce

Use an unprivileged Linux x86_64 account, a prepared Rust-enabled `vm_api_case`, and a disk-backed ext4, XFS or btrfs work directory. Set `VM` to the absolute runner path from the [guest recipe](guest.md#本地复现). Run from the repository root:

```sh
set -eu
umask 022
mkdir -p benchmark/vm/.data
D=$(mktemp -d "$PWD/benchmark/vm/.data/packed-tree.XXXXXX")
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/staged_tree.c -o "$D/tree"
rustc --edition=2024 -O -Dwarnings benchmark/vm/packed_tree_once.rs -o "$D/once"
TMPDIR="$D" python3 benchmark/vm/check_packed_tree.py --vm "$VM" --probe "$D/tree"
```

The [untimed check](check_packed_tree.py) checks all three complete trees against an independent byte oracle; replaces the pack pathname before import; kills the VM after submission and the importer on either side of rename; supplies an empty conflicting `ready`, short and oversized packs, and a 64 KiB file-size limit producing a real short destination write. These checks passed on btrfs with the #212 Linux x86_64 CI `vm_api_case` (`a49b74917ba3…`, Rust guest, 1 vCPU/256 MiB). Process kills are not host crash tests.

One accepted operation (set `ARM` to `local`, `pack` or `shared`; the trial path **must not exist**):

```sh
timeout -k 3s 30s "$D/once" "$VM" "$D/tree" "$ARM" "$D/trial-001"
```

The single stdout JSON object reports `arm`, `end_to_end_ns`, `setup_ns`, `vm_ns`, and `finish_ns`. One host monotonic clock spans fresh trial/root preparation, probe installation, VM boot, generation, guest export/sync/exit, and host completion through the final sync and importer exit. `end_to_end_ns` is the sum of the three phases; `vm_ns` is not guest CPU time. The driver then verifies the exact names, types, lengths and every byte; failure emits no accepted JSON. Tool compilation, final verification and trial cleanup are outside the clock.

For a formal comparison use randomized complete blocks, one fresh trial and VM per arm, identical binaries, backing filesystem and CPU affinity. Retain all per-arm JSON, stderr/status, hashes and failures. Report paired differences and intervals on `end_to_end_ns`; do not compare the guest's `TREE_PUBLISHED_NS` to `PACK_SUBMITTED` or omit the host import from the candidate. No speed claim is implied by passing the contract checks.

# Does importing immutable input once pay back its cost?

[B-FS-ENG](../REGISTRY.md#b-fs-eng), opt-in Rust-native KVM mechanism experiment. This is the **input** side: host files → guest workload. No guest file is exported, published or written back. It does not change a product mount or a benchmark gate.

Both arms start with the same experiment-owned, stable `part-0`…`part-127` files. The fixture uses the [wide staged-tree layout](../evidence/native-file-handoff.md): 128 × 32 KiB, each 1 KiB block `j` of file `i` filled with `(31*i + j) mod 256`. Even and odd files have modes `0444` and `0440`, respectively.

| Arm | Host preparation from existing files | Guest preparation | Workload root |
|---|---|---|---|
| Direct | None | None | Existing `vm_api_case --readonly` virtio-fs share |
| Pack | Write those actual files in index order to a new pack, flush and close | Open that one shared file and stream it once into a fresh mode-`0700` directory on `/dev/shm` | Private guest tmpfs |

The pack contains 128 records: two little-endian bytes of actual source permission bits followed by the 32 KiB payload (4 MiB + 256 bytes total). It has no encoded paths or lengths. The [guest](read_input_guest.rs) accepts only the two allowed modes, synthesizes the fixed names, and uses one 256 KiB sequential buffer and one 32 KiB part buffer. It checks the read-only virtio-fs and read/write tmpfs mounts. This measures only names, regular-file type, permissions, size, EOF and bytes—not inode numbers, owners, timestamps, xattrs, links or concurrent changes.

The same executable runs the same POSIX workload on the selected directory. **Sparse** opens files 0, 43, 85 and 127 once (128 KiB total, no `readdir`). **Broad** enumerates the exact name set, stats and reopens every file, four times (16 MiB total). A fresh VM runs each cell: sparse never benefits from a preceding scan. Both workloads check every selected byte and its file metadata inside the clock. They return the count and sum of the labels read. The host checks both against its own oracle.

After receiving the result the host acknowledges it; only then does the guest audit the **entire** visible tree against the expected names, types, modes, sizes and every byte, including the 124 unused sparse files. A missing result, wrong answer, incomplete audit, timeout or nonzero exit invalidates the operation. The [driver](read_input_driver.rs) stops the cohort and retains the failed trial.

## Reproduce

On Linux x86_64/KVM, use an unprivileged account, a btrfs/ext4/XFS output directory, GNU `timeout`, and a prepared Rust-enabled [`vm_api_case`](../vm/guest.md). Set `VM` to its absolute path. From the repository root:

```sh
set -eu
umask 022
mkdir -p benchmark/filesystem/.data
D=$(mktemp -d "$PWD/benchmark/filesystem/.data/read-input.XXXXXX")
cp benchmark/filesystem/read_input_*.rs "$D/"
rustc --edition=2024 -D warnings -O --target x86_64-unknown-linux-musl \
  "$D/read_input_guest.rs" -o "$D/probe"
rustc --edition=2024 -D warnings -O "$D/read_input_driver.rs" -o "$D/driver"
sha256sum "$VM" "$D/"*.rs "$D/probe" "$D/driver" > "$D/SHA256SUMS"
rustc -Vv > "$D/rustc"; uname -a > "$D/host"; df -T "$D" > "$D/backing"
taskset -c 4,5 "$D/driver" "$VM" "$D/probe" "$D/smoke" 1 21341
taskset -c 4,5 "$D/driver" "$VM" "$D/probe" "$D/pairs" 15 21342
```

Choose available, distinct physical CPUs and coordinate their use before measuring. The driver uses seeded, randomized complete blocks, 1 vCPU, 256 MiB guest RAM, queue handoff off, and a 30-second VM timeout (3-second kill grace). It does not clear caches, set a cgroup quota, or control host frequency. Source generation, probe installation and root preparation are common and untimed. Every cell has its own root and unused output directory. Retain `samples.csv`, plan, per-trial actual command, stdio, status, source and hashes; never reuse a trial path.

For an untimed rejection check, copy a smoke cell's `input.pack` to a new share and use its command with a new root and that `--readonly` path, feeding a newline through a pipe and capturing task stdout/stderr in pipes. Making the pack one byte short or long, or setting its first mode to `0xffff`, must fail before `RESULT`. For `pack sparse`, change file 1's mode bytes at offset 32,770 from `20 01` to `24 01` (octal `0440` → `0444`), or change the payload byte at offset 32,974 to `ff`. Neither changes the sparse answer (`4 9184`); the full audit must still fail with nonzero exit and no `AUDIT` marker. Do not mutate timed inputs.

| CSV clock | Boundary |
|---|---|
| `build_ns` | Host creates pack directory and file, reads input files, writes pack and finishes closing. Zero for direct. Immediate consumption; no `fsync` or durability claim. |
| `import_ns`, `work_ns` | Guest monotonic clocks. Import includes framing, private directory, transfer, all file creation and close. Work includes the identical read/verification loops. |
| `vm_result_ns` | Host immediately before VM launch through receipt of its result line, including boot and guest import. A **ready pack** starts here. |
| `cold_result_ns` | Host immediately before pack build (no work for direct) through that same result, including VM launch. **Cold means no prebuilt pack or existing VM, not a disk cache flush.** |
| `vm_including_audit_ns` | Launch through full-tree audit and VM exit. An acceptance clock, not sparse read latency. |

Compare within a workload and block. Don't subtract aggregate medians, add a median build to a median VM time, or turn the serial buffer size into a FUSE request count. This experiment buys fewer per-file shared operations by reading *all* input up front and allocating guest-private files. It measures neither total memory residency nor a suitable default for mutable shares, workspaces, arbitrarily large inputs, or cross-VM cached packs.

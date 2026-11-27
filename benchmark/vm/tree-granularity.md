# Does tmpfs staging help when the producer already batches writes?

Engineering A/B, [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng), separate from the VM regression gate. Keep the [eight-file publication protocol](packed-tree.md) and its 4 MiB byte pattern fixed. Compare direct shared generation with tmpfs generation plus 256 KiB export at producer write sizes of **1, 16 and 256 KiB** (4,096, 256 and 16 guest `write` calls). These are application calls, not observed FUSE request counts. No host importer in this comparison.

Both arms write only in a disposable, caller-owned namespace. Sync the eight shared files and `.pending`, rename once to a previously absent `ready`, and sync its parent. `ready` is the publication boundary, not `.pending`. The final result must contain only the new directory (mode `0700`) and eight distinct regular files (`0600`, one link); host UID/GID must be 1000:1000 and every byte must match the independent oracle. No concurrent publisher, existing-tree mutation, transparent `:rw`, RAM quota, or power-loss guarantee. In particular, guest `RENAME_NOREPLACE` on this share does not serialize a competing host rename.

## Reproduce on Linux x86_64/KVM

Use a disk-backed ext4/XFS/btrfs checkout and rootless host UID/GID 1000:1000. Set `VM` to a prepared Rust-native `vm_api_case` from the [VM build recipe](guest.md#本地复现). Build the tools before timing; never run other builds on the selected cores during the comparison.

```sh
set -eu
umask 022
mkdir -p benchmark/vm/.data
D=$(mktemp -d "$PWD/benchmark/vm/.data/granularity.XXXXXX")
export PVISOR_BENCH_GUEST="$D/tree" PVISOR_BENCH_TREE_ONCE="$D/once"
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/staged_tree.c -o "$D/tree"
rustc --edition=2024 -O -Dwarnings benchmark/vm/packed_tree_once.rs -o "$D/once"
TMPDIR="$D" taskset -c 2,3 python3 benchmark/vm/check_packed_tree.py --vm "$VM" --probe "$D/tree"
taskset -c 2,3 mise run benchmark -- vm/tree-granularity --binary "$VM" \
    --output "$D/raw" --criterion-output "$D/criterion"
python3 benchmark/vm/summarize_tree_granularity.py "$D"
```

The untimed conflict check stops the producer after local generation, when the shared `.pending` is still empty. It plants an existing `part-3` there before export. The first three files arrive; the exclusive create fails, the conflict survives and no `ready` appears. Failure leaves scratch to clean up. This tests a mid-export failure, not recovery from machine failure.

The [Criterion adapter](../criterion/src/tree_granularity.rs) shuffles sizes and arm order from seed 21316. A complete block is six fresh VMs. Each VM uses the same probe with 1 vCPU/256 MiB. The primary metric is host elapsed time from trial creation to completion (including boot, task exit and final sync); the secondary guest clock spans generation through parent sync. Host verification follows timing but is required for admission. Failure aborts the run, with partial trial files and logs retained. No slow-sample exclusion, cache drop or tail claim.

Criterion measures the sum of the six completed operations, **not** the speedup or time for either arm. Use the ordered `raw/block-*/block.json` records and the paired summary instead. It excludes Criterion warm-up blocks, reports arm medians and the paired median difference (local minus shared) with a seeded 95% percentile bootstrap over complete blocks. Inspect the raw distribution before interpreting medians; no multiple-comparison correction. Results bound these three sizes on this host, not a universal break-even size.

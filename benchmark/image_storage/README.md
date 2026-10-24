# Local image disk inventory

This answers B-IMAGE-STORAGE-ENG: how much allocated storage does the compressed
local pool save for the same immutable OCI image, relative to unpacking and to
Docker's real image store? This is an engineering inventory, not a latency test.

Use a frozen pVisor executable and fixed linux/amd64 manifest digests. Keep source
downloads, logs and binaries outside the measured stores. Record every config
and compressed layer digest and verify its bytes. On Btrfs, disable transparent
compression on the experiment directory before creating stores. Record both
apparent bytes and `st_blocks * 512`, counting hardlinked inodes only once.
These are allocated data/directory blocks, not the filesystem's global metadata
or physical device write amplification. Do not use tmpfs as a disk measurement.

`run.py` uses new private stores and a dedicated rootless Docker daemon, never
the existing host Docker socket. It records Docker's empty baseline and loaded
store separately. Docker uses the containerd overlayfs snapshotter; compressed
content and unpacked snapshots are reported separately. No volume, build cache or
container writable layer is included in the image-only comparison. pVisor's
catalog/index and content pool are all included. Persistent retained objects
count even if no current tag reaches them.

Measure individual images and then an actual combined store. Re-pull is an
idempotence check, not another independent image. Validate a known file and, when
runtime facilities permit, a real workload before accepting each result.
Keep validation side effects in separate stores or remove only private test
containers before measuring again. Record failures and the entire inventory;
do not silently replace failures with successful retries.

Static inventories use the same exact bytes on both sides and report raw byte
counts and any repeated-inventory range. A ratio describes only these selected
images and storage settings; it is not an average over registry images. It says
nothing about mmap performance, cold-read CPU cost, peak import space, deletion
GC or production capacity. Keep raw evidence under `benchmark/.data/`; do not
publish these engineering results into user benchmark pages.

## Batch contract and execution

A fresh batch contains `runtime/pvisor` and `inputs.json`. Each input records
`name`, `reference`, `pinned`, `digest`, `source` and the parsed OCI `manifest`.
`source` points to a verified `skopeo copy --preserve-digests ... dir:...`
directory containing `manifest.json` and config/layer files named by SHA-256.
The runner revalidates every source file before importing. Source directories can
be reused across fresh batches; measured stores cannot.

```sh
python3 benchmark/image_storage/run.py BATCH --repeats 3
python3 benchmark/image_storage/analyze.py BATCH
```

The analyzer additionally reads `legacy-results.json`: one row per image with
its `name` and `rootfs` inventory. Obtain this independent unpacked-tree reference
with the existing `pvisor cache --backend filesystem --location RAW_EXPORT
--image-store LEGACY_STORE publish PINNED_IMAGE` path; count only the selected
`rootfs-v3/sha256/DIGEST` directory. Exclude RAW_EXPORT from all comparisons.
The legacy column excludes compressed source blobs, so it is a conservative
unpacked-tree comparison rather than an old full-store total.

Each trial saves its complete per-inode inventories, source/runtime checks and
logs. `manifest.json` records the frozen executable and harness digests. Retain
the frozen `harness.py` when formatting or otherwise maintaining the source after
measurement. `summary.json` and `summary.csv` contain byte counts and same-input
inventory intervals; three repetitions do not characterize arbitrary images.
After verification, large private stores can be removed while retaining all
inventories, input blobs, frozen artifacts and cleanup records. Deleting those
stores is experiment cleanup and does not establish production image GC.

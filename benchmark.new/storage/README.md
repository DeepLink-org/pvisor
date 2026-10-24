# Storage inventories

`image_storage.json` declares the capacity columns for B-IMAGE-STORAGE-ENG.
It is deliberately unprepared for the generic command timer: allocated bytes
are not milliseconds. The disk inventory runner and its scope are documented
in [benchmark/image_storage](../../benchmark/image_storage/README.md).

Keep manifests, per-file inventories, logs and frozen artifacts in the private
batch's `.data/` directory. Do not publish storage results as execution latency.

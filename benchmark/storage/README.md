# Storage inventories

`image_storage.json` declares the capacity columns for B-IMAGE-STORAGE-ENG.
It is deliberately unprepared for the generic command timer: allocated bytes
are not milliseconds. The legacy disk inventory runner is retired; its historical scope is documented
in [the archived manual](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/image_storage/README.md). No current inventory runner is prepared.

Keep manifests, per-file inventories, logs and frozen artifacts in the private
batch's `.data/` directory. Do not publish storage results as execution latency.

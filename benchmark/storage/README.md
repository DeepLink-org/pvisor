# Storage inventories

`image_storage.json` declares the capacity columns for B-IMAGE-STORAGE-ENG.
It is deliberately unprepared for the generic command timer: allocated bytes
are not milliseconds. The legacy disk inventory runner is retired; its historical scope is documented
in [the archived manual](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/image_storage/README.md). No current Docker-versus-pVisor inventory runner is prepared for that entry.

Keep manifests, per-file inventories, logs and frozen artifacts in the private
batch's `.data/` directory. Do not publish storage results as execution latency.

`image_format.py` is the explicit B-IMAGE-FORMAT-ENG paired storage runner.
It reuses semspec's command evidence and repeat machinery. Freeze two release
Rust test executables containing the same ignored measurement test, retain the
baseline runner source, then invoke it with `--baseline`, `--candidate`,
`--baseline-runner`, and a new `--output .data/NAME`. Formal batches use
`--runs 30 --warmup 3 --cpu N`; vary `--files`, `--directories` and `--backend`.
It validates content/counts before accepting results and saves complete artifacts,
plans, logs, paired timings, object inventories and bootstrap intervals. Hosts
retain their page cache; new reader processes start with empty decoded caches.
This measures storage stages, not VM startup. Filesystem compression and allocation
accounting are recorded; st_blocks is not exclusive physical device consumption.

# Lab notes

This section keeps engineering experiment records for performance and mechanisms: measurements, analyses, and failure reviews made during design. They answer "why was it designed this way", not "which option should you choose now"—for selection decisions, see [benchmarks and comparisons](../benchmarks/index.md).

Numbers on each page belong to their frozen cohorts and artifact versions and do not represent the performance level of the current build. `.data/` paths point to local archives that are not published with the repository or site; they exist for cohort provenance only.

Complete CSV/JSON, provenance summaries and raw records remain in each batch's local `.data/` or publication directory. Previously public data files are archived under `benchmark.new/.data/docs-assets-prune-20261018/`; the migration inventory retains original paths, byte hashes and archive locations.

| Page | Contents |
| --- | --- |
| [VM startup performance analysis](vm-startup-performance-analysis.md) | Startup-path breakdowns, regressions, and acceptance records |
| [End-to-end task performance analysis](agent-task-performance-analysis.md) | End-to-end measurements of real agent tasks across executors |
| [VM memory performance analysis](vm-memory-performance-analysis.md) | Static curves and readback costs for deduplication, shared libraries, and pooling |
| [Filesystem performance analysis](filesystem-performance-analysis.md) | Load measurements and retest records for OverlayFS configurations |
| [Cache and asynchronous-write performance curves](filesystem-io-performance-analysis.md) | Engineering curves for request size, concurrency, synchronization, and write coalescing |
| [Benchmark methodology and evidence](benchmark-methodology-evidence.md) | Measurement scope and evidence inventory of historical cohorts |

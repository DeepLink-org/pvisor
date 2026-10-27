# Lab notes

Engineering experiment records preserve performance measurements, mechanism analyses and failure reviews to explain past design choices. For current executor choices, use [benchmarks and comparisons](../benchmarks/index.md).

Numbers on each page belong to their frozen cohorts and artifact versions and do not represent the performance level of the current build. `.data/` paths point to local archives that are not published with the repository or site; they exist for cohort provenance only.

Complete CSV/JSON, provenance summaries and raw records remain in each batch's local `.data/` or publication directory. Previously public data files are archived under `benchmark/.data/docs-assets-prune-20261018/`; the migration inventory retains original paths, byte hashes and archive locations.

| Page | Contents |
| --- | --- |
| [VM startup performance analysis](vm-startup-performance-analysis.md) | Startup-path breakdowns, regressions, and acceptance records |
| [End-to-end task performance analysis](agent-task-performance-analysis.md) | End-to-end measurements of real agent tasks across executors |
| [VM memory performance analysis](vm-memory-performance-analysis.md) | Static curves and readback costs for deduplication, shared libraries, and pooling |
| [OpenSandbox: startup, disk and memory](opensandbox-performance-analysis.md) | Native guest creation wait, compressed image inventory and complete service-group memory |
| [V2 versus native Docker: storage, startup and memory](image-format-performance-analysis.md) | Same-digest native Docker inventories, CLI readiness and complete deployment memory |
| [Filesystem performance analysis](filesystem-performance-analysis.md) | Load measurements and retest records for OverlayFS configurations |
| [Cache and asynchronous-write performance curves](filesystem-io-performance-analysis.md) | Engineering curves for request size, concurrency, synchronization, and write coalescing |
| [Benchmark methodology and evidence](benchmark-methodology-evidence.md) | Measurement scope and evidence inventory of historical cohorts |

| [Memory offload during Agent waits](agent-offload-performance-analysis.md) | Fixed SWE replay, Host wait management and memory/latency tradeoffs |

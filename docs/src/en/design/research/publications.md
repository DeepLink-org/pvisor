# Papers, reports, and talks

pVisor's public technical material currently consists of repository designs and engineering experiments. Use these records to understand execution mechanisms, select experiment subjects, and trace results to their source revisions and environments.

## Existing engineering material {#materials}

The [Replay fidelity report](../../benchmarks/replay-fidelity.md) measures short-prefix preparation for pinned trajectory formats: it checks historical boundaries, command arguments and tool observations, and records elapsed time from CLI launch through preparation exit. `prepare-only` executes no tools, starts no Agent and leaves the workspace unchanged. Use these results to select historical context inputs; the [Replay design](../replay.md) explains the responsibilities of prefix preparation, tool re-execution and live continuation.

The [RAM offload appendix](../offload/index.md#experiments) retains the 2026-10-03 storage/control checks and a compressed backing sample. Use it to study publication order, base/delta inheritance and storage-size accounting. The record separately lists passing mechanism checks, tests blocked before guest execution, and the sample files' logical sizes and allocated space.

Retired [Controller/Worker evidence](../cluster-performance-analysis.md) provides historical measurements of frozen artifacts. Readiness probes describe VM readiness time; history-query measurements describe query and warm-replay costs for retained records. Completed-task cohorts retain their own provenance and statistical definitions. Cite data separately by experiment subject and cohort.

Specific measurement gaps: the replay report has not measured fidelity of continuation with a real model; the offload sample has not measured net physical-memory savings or multi-VM density; current daemon performance and density await independent experiments.

## Record a new public output {#record}

Each entry needs title, authors or speaker, publication date, public link, material type, source commit, agent/model versions, data and reproduction entry, and conclusion scope. Papers should include venue and DOI; identify preprints, engineering reports, and talks by type.

If research uses an unreleased branch, record its commit and differences from the release. Preserve the original record when implementation changes, adding new validation or a superseding explanation.

## How to cite {#citation}

Cite performance conclusions with report date, hardware, executor, task, and statistical definitions. Cite designs with a source commit or release version. Retain the environment conditions for effects observed in a specific environment.

No formal paper, external report, or talk is currently registered here. Contributions should provide accessible links and reproduction details using the fields above. Distinguish a design proposal, a passing mechanism test and a measured workload result when citing them; implementation guarantees belong to the matching version's documentation and tests.

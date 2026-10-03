# Papers, reports, and talks

Start with design documents and reproducible experiments to explore pVisor's public technical material. This index currently lists repository engineering material. Formal papers, talks, and external research reports should be added with authors, dates, and matching implementation versions when published.

## Existing engineering material {#materials}

| Material | What it helps assess | Version mapping |
| --- | --- | --- |
| [Design principles](../principles.md) | Execution core and optional component boundaries | Source commit containing the document |
| [Evidence design](../../concepts/capabilities-and-evidence.md) | Requests, plans, and installed controls | Bundle schema and source commit |
| [Replay design](../replay.md) | Tool re-execution and native session reconstruction | Pinned adapter and agent versions |
| [Startup experiments](../../benchmarks/startup.md) | Startup cost on the specified machine and configuration | Report date, commit, and firmware hash |
| [Measurement methodology](../../benchmarks/methodology.md) | Reproducing and interpreting performance experiments | Harness commit and raw report |

These materials support implementation understanding and experiment reproduction. Cite the environment and version scope from the original report when using its data.

## Record a new public output {#record}

Each entry needs title, authors or speaker, publication date, public link, material type, source commit, agent/model versions, data and reproduction entry, and conclusion scope. Papers should include venue and DOI; identify preprints, engineering reports, and talks by type.

If research uses an unreleased branch, record its commit and differences from the release. Preserve the original record when implementation changes, adding new validation or a superseding explanation.

## How to cite {#citation}

Cite performance conclusions with report date, hardware, executor, task, and statistical definitions. Cite designs with a source commit or release version. Retain the environment conditions for effects observed in a specific environment.

No formal paper, external report, or talk is currently registered here. Contributions should provide accessible links and reproduction details using the fields above; implementation guarantees belong to the matching version's documentation and tests.

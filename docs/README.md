# PolicyVisor Documentation

Use **PolicyVisor** for the product name, **pVisor** for its short name, and
`pvisor` for the CLI. The positioning is **scaling autonomous agent execution**;
in Chinese, **让自主 Agent 的执行可以规模化**, with the day-one line
**让 Agent 全自动执行，文件改动由你决定去留** / "run agents unattended, keep only
the file changes you approve". The product covers Agent CLIs, scripts, and
automation commands; describe Agent-specific integrations as such.

Use `pvisor` for the Python distribution, import package, and wheel filename
prefix. Use current Rust crate names, `PVISOR_*` environment variables, and
repository/deployment URLs. Separate requested policy, installed controls, and observed
results; distinguish ordinary host write-through from `--safe` staging and
describe each executor's platform-dependent boundary.

The site uses Zensical 0.0.61. Chinese design, guides, concepts and reference pages under `docs/src/zh/` are
authoritative. English under `docs/src/en/` contains only the homepage, getting
started and CLI reference; link to Chinese for other subjects.

```bash
just docs-serve         # build, watch, and serve on 127.0.0.1:3000
just docs-serve --port 3001
just docs-build         # build docs/site and validate generated pages
```

`scripts/build-docs.py` renders the canonical Chinese navigation and a smaller
English navigation with each locale's native theme. The language selector opens
the matching article when it exists, otherwise the English homepage. Former
English article URLs redirect to the Chinese originals. `check-docs.py` validates
links, navigation and HTML language attributes.

`docs/overrides/home.html` overrides the native content block for the full-width
homepage. The header, mobile drawer, sidebars and table of contents remain
Zensical components. `docs/src/stylesheets/extra.css` supplies the shared blue
gradient, grid, brand contrast and homepage layout. Use native Markdown fences,
`!!! note` / `!!! tip` callouts, and relative image paths.

CI uses the same bilingual build and page checks before uploading `docs/site`.

## Layout

`docs/src/` is the published site source: `assets/`, `stylesheets/`, `index.md`,
and the per-locale trees `zh/` (authoritative) and `en/` (entry pages only). The
`docs/` root holds only site infrastructure — `zensical.toml`, `redirects.json`,
`overrides/`, and this file. Keep each document next to what it serves: tool and
specification documents live with their tool (for example `tools/semspec/DESIGN.md`),
and user-facing protocol pages belong in the site's `reference/` tree, not at the
`docs/` root. `docs/site/` is generated output; never edit it by hand.

## Information architecture

The canonical Chinese documentation uses six directories:

- `start/`: product scope, installation and the first reproducible Job.
- `guides/`: tasks, commands, expected results and troubleshooting.
- `concepts/`: terminology, evidence and capability limits.
- `reference/`: CLI options and concrete cases.
- `development/`: contributor setup, validation, release and roadmap.
- `design/`: current implementation ownership and mechanisms.

Keep one canonical article per subject and link to it instead of repeating its
contract. Start task guides with prerequisites and executable examples. Separate
implemented behavior from design goals; describe a guarantee only with its
executor and scope. Review/apply examples must enable staging with `--safe`,
`--ask` or `--stage`. If a path is specified, put it outside the project.

The CLI reference owns staging/storage defaults and parameter grammar; the
network guide owns the executor boundary matrix; the evidence page owns the
meaning and limits of observations. Other articles link to these definitions.
Implementation pages explain mechanisms and code ownership. Benchmarks keep
dated samples and reproduction limits under `benchmark/`; speculative designs
do not belong in the current product navigation or search index.

`zensical.toml` owns navigation. Maintain Chinese articles; translate only the English entry points.
`redirects.json` maps old locale-relative Markdown paths to their replacements;
the build emits redirects for English, Chinese, and the original unprefixed
published URLs. Update incoming source links to canonical paths as well.
The checker rejects missing Chinese originals, missing or duplicate navigation targets,
broken links, missing anchors and invalid redirect targets.
Use explicit heading IDs for links to translated sections, so wording changes do not break anchors.
Appendices may be reached through an index without appearing in the main navigation.
Keep semantic case IDs, assertions and annotations intact; passing a case does
not authorize edits to human approval ledgers or snapshots.

## Core design

- [Core architecture](src/zh/design/architecture.md): core owns definitions; pvisor owns scheduling and execution; drivers implement actual boundaries.
- [Operation and Event](src/zh/design/operations-events.md): requests, actual rewrites, Placement, outcomes, causal facts and reconstruction limits.
- [Design principles](src/zh/design/principles.md): ownership, causality and evidence rules.

These articles describe the current implementation. Keep field and sequencing contracts in the Operation/Event article rather than duplicating them in component guides.

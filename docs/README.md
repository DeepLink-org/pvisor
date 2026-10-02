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

The site uses Zensical 0.0.61. Each article under `docs/src/zh/` has a matching
English article at the same relative path under `docs/src/en/`. Chinese defines
the canonical information architecture; both languages describe the same
behavior, examples, evidence and known limitations.

```bash
just docs-serve         # build, watch, and serve on 127.0.0.1:3000
just docs-serve --port 3001
just docs-build         # build docs/site and validate generated pages
```

`scripts/build-docs.py` renders the same navigation in both languages with each
locale's native theme and search index. The language selector keeps the current
article; navigation, breadcrumbs and search results stay in that language. Former
article URLs redirect to their replacements in the same language; unprefixed
legacy URLs use Chinese. `check-docs.py` checks translation coverage, paired
revisions, examples, links, navigation and HTML language attributes.

`docs/overrides/home.html` overrides the native content block for the full-width
homepage. The header, mobile drawer, sidebars and table of contents remain
Zensical components. `docs/src/stylesheets/extra.css` supplies the shared blue
gradient, grid, brand contrast and homepage layout. Use native Markdown fences,
`!!! note` / `!!! tip` callouts, and relative image paths.

CI uses the same bilingual build and page checks before uploading `docs/site`.

## Layout

`docs/src/` is the published site source: `assets/`, `stylesheets/`, `index.md`,
and the complete per-locale trees `zh/` and `en/`. The `docs/` root holds site
infrastructure — `zensical.toml`, `redirects.json`, `translations.json`,
`overrides/`, and this file. Keep each document next to what it serves: tool and
specification documents live with their tool (for example `tools/semspec/DESIGN.md`),
and user-facing protocol pages belong in the site's `reference/` tree, not at the
`docs/` root. `docs/site/` is generated output; never edit it by hand.

## Information architecture

Value before mechanism, mechanism before implementation. The top-level order is
why → start → guides → concepts → benchmarks → security → reference → design → community.

The canonical Chinese documentation uses these sections:

- `why/`: the case for pVisor — vision, supervision-bandwidth argument, trust ladder, use cases, comparisons, when not to use.
- `start/`: product scope, installation, the first reproducible Job, and where to go next.
- `guides/`: tasks, commands and expected results, grouped by job (`agents/`, `policies/`, `executors/`).
- `concepts/`: the user mental model — jobs, staging, evidence, policy, glossary.
- `benchmarks/`: reproducible measurements and dated comparisons.
- `security/`: what is protected, what is not, and how to report issues.
- `reference/`: CLI, configuration, schemas and platform support matrices.
- `design/`: implementation ownership, mechanisms, ADRs and research directions.
- `community/`: contributing, development, testing, release, roadmap and policies.

Readers and entry points:

| Reader | Entry | Main path |
| --- | --- | --- |
| Developer using an agent | home | why → first run → connect your agent → review and merge |
| Platform / DevOps | home → use cases | use cases → benchmarks → CI → configuration |
| Security reviewer | security | threat model → executor boundaries → capabilities and evidence → disclosure |
| Researcher (MLSys, agentic RL) | use cases → research | research/training use case → replay and fork → architecture → research directions |
| Contributor | community | contributing → development → testing → architecture |

Keep one canonical article per subject and link to it instead of repeating its
contract. Start task guides with prerequisites and executable examples. Separate
implemented behavior from design goals; describe a guarantee only with its
executor and scope. Review/apply examples must enable staging with `--safe`,
`--ask` or `--stage`. If a path is specified, put it outside the project.

The CLI reference owns staging/storage defaults and parameter grammar; the
network policy guide owns the executor boundary matrix (summarized in
`security/executor-boundaries`); the evidence page owns the meaning and limits of
observations. Other articles link to these definitions. Implementation pages
explain mechanisms and code ownership. Benchmark and comparison pages state
method, environment and date, and keep dated samples under `benchmark/`.

### Planned (TODO) pages

A required page that has no data yet is created with a uniform template and
front matter:

```yaml
---
status: todo
search:
  exclude: true
---
```

It carries a plain-language question, the required metric, controls, workload,
environment, acceptance criteria, and a tracking issue. TODO pages may appear in
the navigation with a “（规划中）” suffix, but they are excluded from search and
are never linked as finished content — the enclosing index says “建设中” and
links to the placeholder instead. Finish a page by removing `status: todo` and
closing its issue.

`zensical.toml` owns navigation. Maintain paired Chinese and English articles.
`redirects.json` maps old locale-relative Markdown paths to their replacements;
the build emits redirects for English, Chinese, and the original unprefixed
published URLs. Update incoming source links to canonical paths as well.
The checker rejects missing Chinese originals, missing or duplicate navigation targets,
broken links, missing anchors and invalid redirect targets.
Use explicit heading IDs for links to translated sections, so wording changes do not break anchors.
Appendices may be reached through an index without appearing in the main navigation.
Keep semantic case IDs, assertions and annotations intact; passing a case does
not authorize edits to human approval ledgers or snapshots.

### Bilingual maintenance

For a new article, create both locale files at the same relative path. For a
behavior change, update both versions in the same change. Translate all prose,
including prerequisites, expected results, platform limits and TODO acceptance
criteria. Keep executable fences identical; translate surrounding explanations
and `text` diagrams. Preserve explicit heading IDs, semantic case IDs, page
status and search exclusion. Links within an article should stay in its locale.

After reviewing both versions, record the pair and build:

```bash
python3 scripts/check-docs.py --record-translations
just docs-build
just test-py tests/test_docs.py
```

`translations.json` stores a combined SHA-256 of each pair. Changed or missing
articles fail the build until both versions are reviewed and recorded. Recording
also checks executable examples, explicit IDs, status and search exclusion.
These mechanical checks detect drift; they do not prove that translated prose
has the same meaning. Review commands, numbers, versions, claims and limitations
before recording. This revision file is not a semspec approval ledger and never
replaces human semantic approval.

Navigation labels for English groups live in `EN_NAV_LABELS` in
`scripts/build-docs.py`; ordinary article labels use translated page titles.
Add the English label when adding a Chinese navigation group. Planned pages
remain planned in both languages until their original acceptance criteria are
met; do not fill missing measurements, audit results or maintainer decisions
with inferred claims.

## Core design

- [Core architecture](src/zh/design/architecture.md): core owns definitions; pvisor owns scheduling and execution; drivers implement actual boundaries.
- [Operation and Event](src/zh/design/operations-events.md): requests, actual rewrites, Placement, outcomes, causal facts and reconstruction limits.
- [Design principles](src/zh/design/principles.md): ownership, causality and evidence rules.

These articles describe the current implementation. Keep field and sequencing contracts in the Operation/Event article rather than duplicating them in component guides.

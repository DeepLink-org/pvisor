# PolicyVisor Documentation

Use **PolicyVisor** for the product name, **pVisor** for its short name, and
`pvisor` for the CLI. The positioning is **scaling autonomous agent execution**;
in Chinese, **让自主 Agent 的执行可以规模化**, with the subtitle
**提升执行密度，降低监督成本。** / **Higher execution density. Lower supervision cost.**
Describe the two supporting paths: resource sharing and idle-memory reclamation
for machine capacity; staged file changes, execution records and review for
supervision efficiency. Quantified gains require workload-specific evidence.
The product covers Agent CLIs, scripts, and
automation commands; describe Agent-specific integrations as such.

Use `pvisor` for the Python distribution, import package, and wheel filename
prefix. Use current Rust crate names, `PVISOR_*` environment variables, and
repository/deployment URLs. Separate requested policy, installed controls, and observed
results; distinguish ordinary host write-through from `--safe` staging and
describe each executor's platform-dependent boundary.

The site uses Zensical 0.0.67. Each article under `docs/src/zh/` has a matching
English article at the same relative path under `docs/src/en/`. Chinese defines
the canonical information architecture; both languages describe the same
behavior, examples, evidence and known limitations.

```bash
just docs-serve         # native Chinese preview on 127.0.0.1:3000
just docs-serve en      # native English preview on 127.0.0.1:3001
just docs-serve zh -a 127.0.0.1:3002
just docs-build         # clean-build both languages and validate generated pages
just docs-build --require-recorded  # same build with CI's strict translation check
```

The recipes call `zensical build` and `zensical serve` directly. Configuration is
checked-in native TOML. Define navigation as a single nested `nav = [...]` array
under `[project]`, following Zensical's official navigation examples. Use inline
tables for labels and nested arrays for sections and groups, without repeated
`[[project.nav...]]` headers. No configuration is generated at build time, and no
Python build wrapper, source copy, temporary configuration, HTML rewriting,
custom search index or custom HTTP server is involved.

Zensical has one theme language and search index per project. Keep exactly two
native configurations: `zensical.zh.toml` and `zensical.en.toml`, building each
locale directly into `site/zh` and `site/en`. `docs/index.html` is a small static
language selector copied unchanged to the published root; it needs no third
Zensical project. Builds start with a clean `docs/site/` to remove stale outputs.
Both locale configurations own their translated navigation and native search.
The language selector keeps the current article. Preview one locale at a time
with the native server; alternate-language links use absolute published-site URLs, preserving the article
path. A local preview serves one locale, so switching language opens the published
translation. To inspect unpublished translations, preview the other locale
separately (`just docs-serve en` or `just docs-serve zh`).
`check-docs.py` checks translation coverage, paired revisions, examples, links,
navigation, HTML language attributes and that every article was generated.

`docs/overrides/home.html` overrides the native content block for the full-width
homepage. The header, mobile drawer, sidebars and table of contents remain
Zensical components. `docs/overrides/assets/stylesheets/extra.css` supplies the shared blue
gradient, grid, brand contrast and homepage layout. Use native Markdown fences,
`!!! note` / `!!! tip` callouts, and relative image paths.

The Zensical version is pinned once in `justfile`. CI runs the documentation
regression tests and `just docs-build --require-recorded` before uploading
`docs/site`; do not duplicate the build commands in the workflow.

## Layout

`docs/src/zh/` and `docs/src/en/` contain the complete locale source trees and
their product homepages. `docs/overrides/assets/` holds shared static resources
that Zensical publishes natively in each project. The `docs/` root holds site
infrastructure — the two locale TOML files, the static `index.html` language
selector, `translations.json`, `overrides/`, and this file. Keep each document next to what it serves: tool and
specification documents live with their tool (for example `tools/semspec/DESIGN.md`),
and user-facing protocol pages belong in the site's `reference/` tree, not at the
`docs/` root. `docs/site/` is generated output; never edit it by hand.

## Information architecture

Value before mechanism, mechanism before implementation. Keep Home as the first
top-level tab, followed by six sections: Quick start → User guide → Core concepts
→ Benchmarks → Design and research → Contributing
(首页 → 快速开始 → 用户指南 → 基础概念 → 基准测试 → 设计与研究 → 参与开发).
Quick start includes the product introduction and `why/` articles; User guide
includes the task learning path and reference pages; Core concepts includes
Security as a subsection. The homepage remains accessible through its own tab
and the site logo. This grouping changes navigation, not article
paths or the separation between tutorials, guides, references and explanations.
Within each tab, keep the sidebar to two levels: topic group → article. Flatten
subgroups into their parent group; do not add a third sidebar level.

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
method, environment and date, and keep dated raw samples in local `.data/` directories.
Only derived Markdown tables and same-directory CSV downloads are versioned.
The native `exclude` plugin excludes `.data/` at every depth, and the generated
site checker rejects leaked evidence. Zensical publishes the shared theme assets
in each locale, including images and JSON downloads, so native previews need no
extra server. JSON fixtures, recording tools and published figures use
`docs/overrides/assets/` directly. Existing local evidence remains in the real
`docs/src/assets/benchmarks/.data/` directory, outside the three projects' article
roots. Do not replace the former tracked `docs/src/assets/` or
`docs/src/stylesheets/` directories with symlinks: Git cannot stage individual
old-file deletions through a symlink during the migration.

### Publication readiness

A reader-facing article is ready when it answers its question using current,
verified behavior, provides usable examples where appropriate, and clearly
states the scope of available evidence. Completing documentation does not
establish a new product guarantee, approve an ADR, adopt a governance policy,
or create benchmark or audit results.

Keep engineering requirements and contributor acceptance checklists in this
file or their tracking issues. Published pages describe the available interface
and what readers can do with it. Research pages distinguish implemented
integration points from proposed extensions; audit and adopter pages state the
current public record without inventing reports or participants.

Assess completion against the page's acceptance requirements, not its front
matter or navigation label. Removing `status: todo` does not complete a missing
field reference, schema, experiment, or policy decision. Keep outstanding items
explicit and attach the implementation or verification that completes each one.
Preserve page paths and bilingual parity. Keep both locale navigation trees in
the Home-plus-six-section order defined above.

### Writing style

Write for a developer who is deciding whether to adopt pVisor and then how to use
it. The models are the uv, Ruff and Ray docs.

- Lead with the outcome or the command, then explain. Short paragraphs, one idea each.
- Describe purpose, behavior and scope directly. Avoid rhetorical contrasts such as
  “不是……而是……” / “not X, but Y”, imagined objections and repeated disclaimers.
  Express limitations as concrete prerequisites, unsupported operations, verification
  gaps or recovery steps. Preserve safety boundaries and implementation status.
- Second person, active voice. “Run this”, “pVisor refuses to overwrite your edit”.
- Prefer a concrete example over an abstract description; show the command and what it does.
- State a guarantee once, on the page that owns it, and link from elsewhere. Never open a page with a disclaimer, and never repeat a caveat in every paragraph.
- No meta text: no “本文/本节/本页”, no “继续阅读”, no first line that restates the heading.
- Name the mechanism when it matters; keep terms consistent with the glossary.
- Tables for options, matrices and comparisons; code fences for anything runnable.
- Keep article directories in the sidebar. Omit tables that only map topics to
  documentation links, and do not replace them with equivalent link lists.
  Index pages should explain the design or usage directly: ownership, execution
  flow, decisions, or actionable steps. Avoid empty section introductions that
  only describe what other pages contain. Retain tables with substantive
  responsibilities, prerequisites, status or measurements.
- Keep overview and topic pages in a summary-to-detail relationship. An overview
  should stand alone with the system model, lifecycle and key tradeoffs. Topic
  pages own protocol fields, algorithms and platform constraints. Link to a topic
  where its detail becomes relevant instead of duplicating its sections.
- A page with no data yet says “建设中” and links to its TODO page instead of making an empty claim.

The native TOML files own navigation. Maintain paired Chinese and English articles.
Keep only current article paths; do not maintain historical redirect maps or
compatibility pages. When moving or merging articles, update incoming source
links and both locale navigations. Old published article URLs are retired.
The checker rejects missing Chinese originals, missing or duplicate navigation
targets, broken links, missing anchors and generated redirect pages.
Use explicit heading IDs for links to translated sections, so wording changes do not break anchors.
Appendices may be reached through an index without appearing in the main navigation.
Keep semantic case IDs, assertions and annotations intact; passing a case does
not authorize edits to human approval ledgers or snapshots.

### Bilingual maintenance

For a new article, create both locale files at the same relative path. For a
behavior change, update both versions in the same change. Translate all prose,
including prerequisites, expected results, platform limits and evidence scope. Keep executable fences identical; translate surrounding explanations
and `text` diagrams. Preserve explicit heading IDs, semantic case IDs, page
status and search exclusion. Links within an article should stay in its locale.

After reviewing both versions, record the pair and build:

```bash
python3 scripts/check-docs.py --record-translations
just docs-build
just test-py tests/test_docs.py
```

`translations.json` stores a combined SHA-256 of each pair. Structural
disagreements — a missing page, divergent anchors, executable examples, IDs,
status or search exclusion — always fail the build. Changed but unrecorded pairs
only warn locally, so you can build and review; CI runs
`check-docs.py --require-recorded`, so a pull request still fails until both
versions are reviewed and recorded. These mechanical checks detect drift; they
do not prove that translated prose has the same meaning. Review commands, numbers,
versions, claims and limitations before recording. This revision file is not a
semspec approval ledger and never replaces human semantic approval.

Maintain matching navigation paths in `zensical.zh.toml` and `zensical.en.toml`.
The root navigation links to both locale homepages. Update incoming links and navigation
when moving an article; do not add historical redirects. Documented pages become searchable in both
languages together. Do not fill
missing measurements, audit results or maintainer decisions with inferred claims;
state the current public record and keep engineering follow-ups below.

### Build reports success but produces no articles

Zensical requires available inotify watches even for a one-time build. On this
Linux host, Cursor occupied about 270,412 watches against a 271,298 limit;
`inotify_add_watch` returned `ENOSPC`, while Zensical exited successfully without
reading the article tree. This is a host resource failure, not a Markdown error.
The page checker must still run after the generator.

`just docs-build` and `just docs-serve` check available native watches before
invoking Zensical. A failed preflight stops before deleting the existing site.
The check cannot reserve capacity against concurrent editor activity.

Check `cat /proc/sys/fs/inotify/max_user_watches`. Free unnecessary editor watches
or increase the host limit, for example `sudo sysctl -w fs.inotify.max_user_watches=524288`.
This command applies until reboot; permanent system policy belongs to the host
administrator. Do not work around the failure with source copies or HTML rewrites.

## Core design

- [Daemon quickstart](src/zh/guides/daemon/index.md): single-node sandbox management, the pinned OpenSandbox API profile and prepared-image prerequisites; [English](src/en/guides/daemon/index.md).
- [Daemon architecture](src/zh/design/daemon/index.md): local admission, lifecycle, durable ownership, APIs and recovery boundaries; per-sandbox supervisors embed native pVisor VM execution; [English](src/en/design/daemon/index.md).
- [Core architecture](src/zh/design/architecture.md): core owns definitions; pvisor owns scheduling and execution; drivers implement actual boundaries.
- [Operation and Event](src/zh/design/operations-events.md): requests, actual rewrites, Placement, outcomes, causal facts and reconstruction limits.
- [Design principles](src/zh/design/principles.md): ownership, causality and evidence rules.

These articles describe the current implementation. Keep field and sequencing contracts in the Operation/Event article rather than duplicating them in component guides.

## Engineering requirements and follow-ups

These requirements remain engineering or policy work. They are separate from
publication readiness and do not authorize AI to approve semantic claims or
make maintainer decisions.

### Unadopted documentation proposals

The following ideas are retained for engineering evaluation only. They are not
publication requirements, adopted study protocols, existing issue automation,
or evidence of completed research:

- Supervision-cost research: consider a fixed-task user study with at least ten
  participants and a preregistered protocol; a smaller internal pilot could
  first assess feasibility. Study design and adoption remain open decisions.
- Comparison corrections: consider a dedicated correction issue template and
  an entry point from comparison articles; no template or workflow is claimed
  to exist by retaining this proposal.
- TODO tracking: consider one GitHub issue per outstanding documentation page,
  a `docs-todo` label, page links to its owner/issue, a build-generated TODO list,
  and coordinated page completion/issue closure. This does not adopt the old
  proposal's navigation or search policy, and removing a TODO marker alone
  does not establish completion.

### Verified reference and example coverage

- `reference/config`: all 100 fields in the configured serde structures, including
  nested mount, route, resource, and policy entries, now have names, types,
  defaults, and usage. `scripts/check-reference.py` compares names and types
  against source during `just docs-build`; Rust documentation tests compare raw
  defaults against serialization and parse the actual TOML fences. CLI precedence
  remains behavioral review, with existing `cli_lists_replace_config_lists` and
  file-access-priority tests. This covers fields, not a complete TOML schema.
- `reference/run-bundle`: 78 fields across the Bundle's own structs, environment,
  lineage, and ChangeEntry have source-checked names/types; presence rules were
  reviewed against serde attributes.
  Linked nested protocols remain owned by their definitions; this does not claim
  a full generated JSON Schema.
- `reference/json-output`: status, review context, kill, and all five checkpoint
  success envelopes are documented. Six downloadable outputs are collected by
  `scripts/record-doc-json.py` from a real offline host task, nonzero exit, and
  deadline. Provenance includes binary/source hashes and actual return codes.
  Rust tests read the samples using the product reader and reject incompatible
  versions and missing required receipts. Generated schemas for all nested
  protocols and the internal whiteout sample-path issue remain outstanding.
- `guides/ci` and `reference/exit-codes`: copyable failure/timeout commands have
  expected return codes, retained-file behavior, and real output samples.
  `tests/documentation_json.rs` executes both paths and checks that candidates
  remain staged. This is local Linux coverage, not a hosted-runner measurement.

Validate content and build before updating bilingual revision hashes:

```bash
just test pvisor
just test-py tests/test_docs.py
python3 scripts/check-docs.py --record-translations
just docs-build
```

To validate a TOML file without launching an executor:

```bash
cargo run --locked -p pvisor --example documentation_config -- run.toml
```

### community/adopters.md

Measurements, generated references, or formal decisions still need completion. The acceptance criteria below remain required; the page retains its planned status.

**Acceptance criteria**

- Publish users and scenarios with consent.
- Distinguish production use from experiments.

### community/code-of-conduct.md

**Requirements**

- Adopt [Contributor Covenant 2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/) or an equivalent text;
- Specify a reporting channel (dedicated email or maintainers), rather than public issues;
- Describe handling procedures and possible consequences.

**Acceptance criteria**

- Add repository-root `CODE_OF_CONDUCT.md` and keep this page consistent;
- Ensure that the reporting channel works and is listed on [Community](index.md).

### community/governance.md

Measurements, generated references, or formal decisions still need completion. The acceptance criteria below remain required; the page retains its planned status.

**Acceptance criteria**

- Define the maintainer list, decision process and advancement path.
- Describe the human role in semspec approval.

### design/decisions/index.md

**Requirements**

- Create an ADR template (context, options, decision, consequences, status) and a numbering convention (`NNNN-short-title.md`).
- Backfill the existing key decisions, for example:
    - Admission plans stop at `Planned`; enforcement comes only from executor observations.
    - Gateway and replay are separated from the core closure as optional features.
    - `--safe` does not select an executor and refuses to start rather than degrading when it cannot enforce.
    - The human approval process for semantic specifications.

**Acceptance criteria**

- At least four numbered ADRs with titles/status.

### design/replay.md

**Requirements**

- Read trajectory, replay complete batches through after_step, rebuild context, continue.
- Adapter mechanisms and limits, including Claude Code, Codex, OpenCode.
- Boundary between replay and continuation and effects not reproduced.

**Acceptance criteria**

- Code/test references per adapter.
- Link replay-fidelity results.

### design/research/cluster-execution.md

**Requirements**

- Metrics: single-node useful-work density and execution/evidence overhead under fixed resource budgets.
- Controls: native execution and external Kubernetes or Ray orchestration, with pinned versions.
- Workload: independent sandboxes; cross-node placement, retries and workflows remain external.
- Environment: one measured node first; any external multi-node experiment has a separately declared scope.

**Acceptance criteria**

- pVisor defines execution semantics without replacing schedulers.
- Distinguish the daemon's implemented VM-only `NativeRuntime` from proposed integrations; require separate prepared-image and end-to-end SDK validation.
- Align claims with measured density; Cluster/controller measurements are not evidence for the node-local daemon.

### design/research/publications.md

Measurements, generated references, or formal decisions still need completion. The acceptance criteria below remain required; the page retains its planned status.

**Acceptance criteria**

- Link papers/reports/talks.
- State implementation revisions.
- Mark outputs that do not establish product guarantees.

### design/research/rl-execution-substrate.md

**Requirements**

- Metrics: throughput, isolation, reproducibility, fork cost.
- Controls: RL framework sandboxes.
- Workload: fixed rollouts, failure replay, checkpoint forks.
- Environment: pinned model/tools.

**Acceptance criteria**

- Integration and scope.
- Tested recording, fork, and prefix replay.
- Separate design from the rollout guide.

### guides/ci.md

**Requirements**

- Metrics: wall clock time for one run, resource use, failure rate, and the number of human interventions.
- Control: the same agent running directly in CI.
- Workload: use the agent to repair failing tests or perform routine refactoring.
- Environment: GitHub Actions runners (Linux/macOS), pinned agent versions.

**Acceptance criteria**

- A copyable workflow example with `--safe`, staging paths, and artifact upload.
- Explicit `apply` semantics in CI (who reviews, when it merges).
- Regression coverage for failure and timeout paths.

### guides/parallel-agents.md

**Requirements**

- Metrics: number of concurrent Jobs, CPU/memory cost per Job, tail latency.
- Control: sequential runs; Docker at the same density.
- Workload: a fixed task set at 8/32/128 concurrent Jobs.
- Environment: one set each for Linux (KVM/FUSE) and macOS (HVF/macFUSE).

**Acceptance criteria**

- A host concurrency limit and resource model.
- A reproducible batch-review flow (aggregate by workspace, apply in batches).
- Alignment with benchmarks/density.

### guides/rl-rollouts.md

**Requirements**

- Metrics: rollout throughput, isolation effectiveness, trajectory reproducibility, fork cost.
- Control: the sandbox components of existing RL frameworks; OpenHands-runtime-style approaches.
- Workload: batch rollouts of fixed tasks, including failure replay and forking from a checkpoint.
- Environment: cluster or multi-host environments; pinned model and tool versions.

**Acceptance criteria**

- The integration with training frameworks and its boundaries.
- Tested behavior for trajectory recording, forking, and tool-prefix replay.
- An explicit relation to design/research/rl-execution-substrate.

### reference/config.md

**Requirements**

- Automatically generate field tables from RunConfig serde definitions.
- Include TOML path, type, default, CLI option, scalar/list merge behavior.
- Identify values not honored on some CLI paths, such as `run.inherit_env`.

**Acceptance criteria**

- Generate at `just docs-build`, or check the generated result against the code in CI.
- Cover `[run]`, top-level `filesystem`, `[overlayfs]`, `[overlaynet]`, `[gateway]`, `[record]`, `[policies.*]`, `[container]` and `[vm]`.

### reference/env-vars.md

**Requirements**

- Collect every read site from the code and generate two tables automatically: variables pVisor reads and variables injected into agents.
- Describe each variable: purpose, default, applicable executors and platforms, and stability.

**Acceptance criteria**

- A script generates the tables and CI detects new `PVISOR_*` read sites that are missing from them.

### reference/exit-codes.md

**Requirements**

- List each subcommand's exit codes and meanings.
- List the main error types (unsupported policy, sandbox setup failure, apply conflict, missing Job) with their exit codes and messages.
- Explain how CI distinguishes "agent failure" from "pVisor refused to run".

**Acceptance criteria**

- The exit-code table maps one-to-one to error types in the code and has test coverage.

### reference/json-output.md

**Requirements**

- Publish a JSON Schema for each command that supports `--json`.
- Mark stable/experimental fields.
- Provide common `jq` queries: whether any access was denied, whether the network boundary is non-bypassable, and whether changes stay within the given paths.

**Acceptance criteria**

- Generate schemas and check output against them in CI.
- Fix the known issue where `status --json` `sample_paths` leaks internal `.wh.d` paths.

### reference/platforms.md

**Requirements**

- Linux x86_64/arm64 and Apple Silicon macOS × host/container/VM × capabilities.
- Stable/Beta/experimental/unsupported maturity labels.
- Every label needs CI/spec/benchmark/issue evidence.

**Acceptance criteria**

- Evidence for every cell.
- README maturity labels link here.

### reference/policy.md

**Requirements**

- Generate field tables from policy type definitions: fields, types, defaults and ranges.
- List the complete rules that the `--safe` preset generates per platform and per Agent command name.
- State the merge rules for each layer and give the test or semantic specification for each rule.

**Acceptance criteria**

- The field table is code-generated or CI-checked.
- The `--safe` preset table matches the `apply_safe_defaults` implementation.

### reference/run-bundle.md

**Requirements**

- Generate a JSON Schema from the Bundle type definitions and publish it with each version.
- Describe each top-level field: source (admission plan, executor observations, OverlayFS, OverlayNet, Gateway), whether it can be `null`, and the difference between `null` and zero.
- State the schema version policy: when it is upgraded and whether old Bundles can be read (old Bundles that lack the observation contract are currently rejected).

**Acceptance criteria**

- Release schema files; check real output in CI.
- Annotated minimal Bundle example.

### reference/stability.md

**Requirements**

- Maintainer-defined version policy, including pre-1.0 incompatible changes.
- Stability and deprecation notice period per interface.
- Old-record handling on Bundle upgrades.

**Acceptance criteria**

- Maintainer confirmation.
- Changelog marks incompatible changes accordingly.

### security/audits.md

**Requirements**

- Metric: audit scope, findings, and remediation status.
- Control group: —
**Acceptance criteria**

- Publish the audit scope, date, and report link.
- Mark each finding with its remediation status.
- Explain the uncovered scope.

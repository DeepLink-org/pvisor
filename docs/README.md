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
method, environment and date, and keep dated raw samples in local `.data/` directories. Only derived Markdown tables and same-directory CSV downloads are versioned. Site builds use a source copy that excludes `.data/` at every depth.

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
Preserve page paths, navigation order, and bilingual parity.

### Writing style

Write for a developer who is deciding whether to adopt pVisor and then how to use
it. The models are the uv, Ruff and Ray docs.

- Lead with the outcome or the command, then explain. Short paragraphs, one idea each.
- Second person, active voice. “Run this”, “pVisor refuses to overwrite your edit”.
- Prefer a concrete example over an abstract description; show the command and what it does.
- State a guarantee once, on the page that owns it, and link from elsewhere. Never open a page with a disclaimer, and never repeat a caveat in every paragraph.
- No meta text: no “本文/本节/本页”, no “继续阅读”, no first line that restates the heading.
- Name the mechanism when it matters; keep terms consistent with the glossary.
- Tables for options, matrices and comparisons; code fences for anything runnable.
- A page with no data yet says “建设中” and links to its TODO page instead of making an empty claim.

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

Navigation labels for English groups live in `EN_NAV_LABELS` in
`scripts/build-docs.py`; ordinary article labels use translated page titles.
Add the English label when adding a Chinese navigation group. Documented pages become searchable in both languages together. Do not fill
missing measurements, audit results or maintainer decisions with inferred claims;
state the current public record and keep engineering follow-ups below.

## Core design

- [Daemon quickstart](src/zh/guides/daemon/index.md): single-node sandbox management, the pinned OpenSandbox API profile and prepared-image prerequisites; [English](src/en/guides/daemon/index.md).
- [Daemon architecture](src/zh/design/daemon/index.md): local admission, lifecycle, durable ownership, APIs and recovery boundaries; per-sandbox supervisors embed native pVisor VM execution; [English](src/en/design/daemon/index.md).
- [Core architecture](src/zh/design/architecture.md): core owns definitions; pvisor owns scheduling and execution; drivers implement actual boundaries.
- [Operation and Event](src/zh/design/operations-events.md): requests, actual rewrites, Placement, outcomes, causal facts and reconstruction limits.
- [Design principles](src/zh/design/principles.md): ownership, causality and evidence rules.

These articles describe the current implementation. Keep field and sequencing contracts in the Operation/Event article rather than duplicating them in component guides.

## Engineering follow-ups from former placeholder pages

These requirements remain engineering or policy work. They are separate from
publication readiness and do not authorize AI to approve semantic claims or
make maintainer decisions.

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

The original acceptance requirements remain below. Measurements, generated references, or formal decisions still need completion, so the planned status remains.

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

The original acceptance requirements remain below. Measurements, generated references, or formal decisions still need completion, so the planned status remains.

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
- Distinguish the daemon's current external Podman adapter from proposed native executor integration.
- Align claims with measured density; retired Cluster measurements are not daemon evidence.

### design/research/publications.md

The original acceptance requirements remain below. Measurements, generated references, or formal decisions still need completion, so the planned status remains.

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

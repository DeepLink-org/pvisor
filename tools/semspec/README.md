# semspec

A standalone Markdown filter and black-box checker. Markdown is the specification;
execution is explicit and human review remains a separate gate. Requires Rust 1.89+, Unix and Bash. Specifications are
trusted executable code; PASS does not establish complete coverage or human approval.
See [the design](DESIGN.md) and [engine contract](ENGINE.md).

```sh
cargo install --path tools/semspec --locked
semspec init cases                   # creates only cases/*.md in a new project
semspec filter cases/example.md              # read-only Markdown → Markdown
semspec filter cases/example.md --execute    # actual output and verdict inline
semspec lint cases
semspec list cases
semspec run cases
semspec run cases/example.md --subject-bin /usr/bin/true
semspec review cases --strict
```

## Markdown filter

```sh
semspec filter cases/example.md --execute > /tmp/example.checked.md
cat cases/example.md | semspec filter --source cases/example.md --stdin
semspec filter cases/example.md --format json  # markdown, byte edits, dependencies, diagnostics
semspec filter cases/example.md --execute --artifacts /tmp/example-evidence
semspec filter cases/example.md --results /tmp/example-evidence --strict
```

No report file is required. Results live in memory by default. `--artifacts` opts
into a new directory containing `report.json`, retained case workspaces and full
stream logs. `--results` reads this report, or JSON from
`run --format json`, without executing. Reports are trusted local observations,
not signed attestations. Source, preparation and engine digests must match to
show a recorded verdict; old or changed evidence is visibly STALE. No result is
NOT RUN. Neither state implies PASS. `--strict` rejects missing/stale evidence.
`--execute` requires every selected case to PASS and returns its failure status
while still rendering the diagnostic. `--require-reviewed` adds the human gate.

The filter preserves frontmatter, prose, links and unmarked fences, removes its
annotation lines and adds ordinary Markdown: case ID, verdict, review state,
stdout/stderr and timing. Output is fenced safely, including embedded fences.
`--stdin` requires `--source` (or INPUT) for relative dependencies; execution also
requires stdin to match the file exactly. JSON format version 1 returns original
UTF-8 byte-offset edits and dependencies for SSG/notebook/watch adapters.
The `experiments` array exposes validated bench IDs, source end offsets, digests
and complete plans in source order, allowing preparation adapters to consume the
same parser without executing subjects or parsing Markdown again. A table
replacement may use a single whole-document edit. Source files are never rewritten.
The Zensical adapter uses this read-only interface; it never runs documentation.
`SEMSPEC_RESULTS` optionally points it at a saved run report.

An optional expected-output cell is part of the same specification:

~~~~markdown
<!-- semspec: case id=S-EXAMPLE-001 -->
```bash
printf 'hello\n'
```

Expected stdout (exact bytes, including the final newline):

<!-- semspec: expect case=S-EXAMPLE-001 stream=stdout -->
```text
hello
```
~~~~

`stream` is `stdout` or `stderr`, with at most one expectation per stream per
case. Expectations must refer to a case in the same document. Cases stay isolated;
use setup blocks for shared fixtures. Filter execution does not introduce a
shared notebook kernel or implicitly execute preceding examples.

No configuration file or external script is needed. Paths are mandatory: semspec
never searches docs/src/zh/cases, cases or cwd implicitly. `run`, `lint`, `list`
and `review` accept one or more Markdown files/directories, and `show ITEM`
accepts paths after the item. `--spec-dir PATH` also supplies explicit inputs and
can be repeated, including for human approval. Directories are scanned recursively;
overlapping files are processed once, while duplicate IDs in distinct files fail.

```sh
semspec run cases/first.md cases/second.md
semspec lint docs/src/zh/cases docs/src/zh/reference
```

~~~~markdown
## Record a successful command

The public command must succeed.

<!-- semspec: case id=S-EXAMPLE-001 timeout=10s -->
```bash
true
```
~~~~

The comment occupies one line immediately before the Bash fence. The ID is required;
`timeout`, paired `xfail-on`/`xfail-reason` are optional. Headings are ordinary prose
and unmarked fences are never executed. One section may contain multiple cases.
The surrounding section, including prose, parameters and examples, is sealed.

Shared preparation is a normal Bash fence preceded by `<!-- semspec: setup -->`.
A case sources preparation from its sibling `index.md`, then its own document,
in source order, in a fresh temporary workspace. Definitions, fixture services and
assertions belong in these Markdown blocks. Their complete documents participate
in review digests. `helper setup FILE.md` prints the extracted preparation for
manual use. Checks call installed commands through PATH; optional `--subject-bin`
(or SEMSPEC_SUBJECT_BIN) supplies an absolute SUBJECT_BIN to checks that use it.
`--timeout` sets the default per-case timeout, otherwise 180 seconds; a comment’s
timeout takes precedence. Environment prerequisites are Bash checks; exit 77 skips.

Each check runs with `set -euo pipefail`, null stdin and its own process group.
Timeout or CLI cancellation sends TERM, then KILL after five seconds; ordinary exit also kills remaining
group descendants. Do not detach persistent processes. Failure directories and
logs are retained, including XFAIL; `--keep` retains successful checks too.
Timeouts, signals and command-not-found/not-executable exits cannot become XFAIL.
PASS/SKIP/XFAIL exit 0 by default; `--require-pass` makes every non-PASS fail.
FAIL/XPASS, inventory errors or required pending review exit 1;
usage/spec errors exit 2; ERROR verdicts exit 3. Human and JSON reports are supported.
SSH signing, JUnit and parallel execution remain unsupported.

The runner requires the report to contain exactly the selected case IDs, once each.
Explicit `--case` IDs must be unique and belong to `--domain` when specified.
With `--output`, valid selection clears the previous report before execution and
publishes the fresh report atomically, even on case failure. Invalid selections
preserve an existing report; launch failure/interruption leaves no stale result.
Output must not replace loaded specifications, preparation or the review ledger.
The USE entry runs semspec with `--require-pass`; no separate catalog parser or
report-validation script is used. Strict PASS does not approve cases and can be
combined with `--require-reviewed` for human review requirements.

`--root PATH` explicitly fixes the review root across selections and requires all
inputs to lie inside it. Without it, the nearest common directory of the supplied directories (or file parents) is
the review root: preparation names are relative to it and REVIEWED.toml lives there.
With a single input this is its directory, as before. A missing ledger means UNREVIEWED. Only a person
may approve from an interactive terminal:

```sh
semspec show S-EXAMPLE-001 cases
semspec --spec-dir cases approve @engine @vocab:index.md S-EXAMPLE-001 --reviewer YOUR_NAME
```

Approval shows current content/dependencies, requires typing the item and atomically
updates only the ledger. Tests never create approval records. Changed case prose,
preparation documents or engine semantics make existing approvals STALE.
TTY checks cannot authenticate a reviewer; repository review rules must protect
specifications, the engine and human ledgers. AI must not execute approve or edit
real ledgers/snapshots, or weaken claims/checks/xfail to obtain PASS.

From pVisor:

```sh
mise run test-semspec
mise run semspec -- lint docs/src/zh/cases
mise run semspec -- run docs/src/zh/cases/01-first-job.md --subject-bin target/release/pvisor
mise run cases -- --suite use               # builds product; semspec requires every selected case to PASS
mise run semspec -- run docs/src/zh/cases --domain USE --require-pass --subject-bin target/release/pvisor
mise run semspec -- review docs/src/zh/cases --strict         # fails until human review
```

STAGE checks live in docs/src/zh/cases/06-stage-apply.md. DOC and VM appendices
use the same Markdown format and preparation from reference/index.md:

```sh
mise run semspec -- lint docs/src/zh/reference
mise run semspec -- lint tools/semspec/semantics
```

Self-specifications in semantics/review.md are drafts. AI must not execute
S-REVIEW-004. Conventional tests cover Markdown inputs, parsing, digest golden
values, review transitions, verdicts, timeout cleanup and exact content/tree helpers.

## Executable benchmark tables

`bench` and `render` work independently of semantic cases and human review.
Define a JSON object whose keys are row names; each row maps column names to
scalar values, or a `$ ` script command that returns the entire row. All resolved rows must have the same columns. JSON row order and the first
row's column order are preserved. Numbers, strings, booleans and null are literal
data; only strings beginning with `$ ` are commands.

```json
{"native": "$ python3 measure.py"}
```

```sh
semspec bench in.json                 # creates in.render.json and in.bench.json
semspec bench --cwd workspace in.json
semspec bench experiment.md           # one inline table; same sibling outputs
semspec render article.md
semspec render article.md --output article.render.md
```

Commands are trusted Bash expressions, run with `-euo pipefail` in the template
folder or explicit `--cwd`, inheriting the caller's environment. A cell command
prints a JSON scalar; a row command prints one nonempty JSON object whose values
are resolved scalars. For example:

```json
{"N": 30, "Median ms": 1.25, "Mean ms": 1.5, "SD ms": 0.1, "Min ms": 1.0, "Max ms": 2.0}
```

The script owns its clock, units and statistics. Semspec executes identical
command strings once per invocation and retains raw returned JSON, logs and
row/cell mappings. It does not time commands, extract external tool exports or
apply pointer/scale transformations. Write diagnostics to stderr. Commands receive
`SEMSPEC_BENCH_LOG`; stdout and stderr are retained in `in.bench.logs/` on failure.

Arrays, empty row objects, nested cells, unresolved `$ ` values, nonzero exits
and inconsistent columns prevent publication. Returned commands are never
recursively executed. Known template errors are checked before execution and
preserve existing results; failures during execution clear old results/evidence.
The template is never overwritten. Successful outputs are written atomically.
A command cannot serve as both a row object and scalar cell.

Keep prose and a script-owned experiment together with an inline JSON table:

````markdown
<!-- semspec: table id=filesystem -->
```json
{"native": "$ python3 measure.py"}
```
````

`filter experiment.md` is read-only and shows NOT RUN. `--format json` exposes
validated `tables` alongside direct-argv `experiments`. `--execute` calls each
distinct expression once per table and renders its returned columns, without
timing or resampling. The script owns timeouts, validation, units and statistics.
Commands use the document directory as cwd. `--artifacts DIR` retains raw JSON,
logs and `report.json`; without it, successful temporary evidence is removed and
failures retain their work directory. `--results DIR` renders offline; changed
source or table engine yields STALE. These observations never approve cases.
For `bench FILE.md`, require exactly one table and no case/bench cells to avoid
silently ignoring another executable experiment; `filter` supports multiple cells.
Registered experiments still use their prepare/audit pipeline before publication.

Row and column insertion order are preserved. Evidence retains `kind: script-row`
or `script-cell`, raw returned JSON, log paths and complete row/cell mappings.
Rendering is read-only, never runs commands, and rejects unresolved values.
`benchmark` separately checks raw trials, hooks, inputs, clock provenance and
recomputed statistics before publishing benchmark measurements.

Table options preserve existing article layouts when moving literal data to JSON:

```markdown
{{ table("data.render.json", {"row_names": false, "markdown": true, "align": ["left", "right"]}) }}
```

`row_names` defaults to true; false displays only the named data columns.
`markdown` defaults to false (escaped text); true explicitly trusts repository
markup such as links, code and emphasis. Pipes and newlines remain table-safe.
`align` is optional and lists `left`, `right`, `center` or `default` for every
rendered column, including row names when enabled. Optional `headers` supplies
one displayed label per rendered column, allowing repeated labels while JSON
keys remain unique. Unknown options, invalid types and incorrect counts fail.

## Shared experiment runtime

Small experiments can live directly in Markdown and render a statistics table:

~~~~markdown
## Python command launch

Observe direct argv launch through normal zero exit; hooks are excluded.

<!-- semspec: bench id=python-launch -->
```toml
argv = ["python3", "-c", "print('hello')"]
warmup = 1
runs = 10
timeout = 5
expected_stdout = "hello\n"
```
~~~~

```sh
semspec filter experiment.md                 # render only; NOT RUN
semspec filter experiment.md --execute       # execute and print the table
semspec filter experiment.md --execute --artifacts /tmp/launch-evidence
```

This path requires Python 3.10+. The runtime is embedded in the installed CLI;
no repository checkout or Python package installation is required. The same
[`runtime.py`](runtime.py) supplies process/replay, Ready/Exit and native filesystem
adapters in `benchmark`. It owns validated repetition, `perf_counter_ns`,
raw trial evidence, statistics and command-trial audits. Experiments own fixture
preparation, subject argv, Ready protocol and correctness checks. Existing JSON
`bench` commands remain supported; complex registered experiments keep their
specialized protocols and publication gates.

Plans accept `argv`, optional `cwd` (relative to the document directory), `env`,
`warmup` (default 0), `runs` (default 30, minimum 2), `timeout` (positive seconds,
default 30), optional `expected_stdout`, and direct-argv `prepare`, `conclude`,
`cleanup` hooks. Shell syntax requires an explicit `bash -c` argv. Preparation and
conclusion must pass for **every** trial, including warmups. Cleanup runs after
failure too; its failure prevents success and cannot hide the original error.
The clock includes subprocess launch, pipe capture and normal exit; hooks,
expected-output checks and aggregation are outside it. No outlier is discarded.
N/median/mean/sample SD/min/max are reported in milliseconds with raw nanoseconds.
A failure stops the plan and emits evidence instead of a partial success table.

Benchmark observations are distinct from human-reviewed cases. Markdown plans
are useful for local probes and engineering checks; they do not bypass registry,
cohort, correctness, immutable-input or publication checks for public benchmarks.
Preparation freezes the shared runtime alongside other harness sources. Historical
measurement data and protocols are not rewritten by this refactor.

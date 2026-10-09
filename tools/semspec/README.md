# semspec

A standalone Rust CLI that extracts human-reviewed, black-box checks from user
Markdown documentation. Requires Rust 1.89+, Unix and Bash. Specifications are
trusted executable code; PASS does not establish complete coverage or human approval.
See [the design](DESIGN.md) and [engine contract](ENGINE.md).

```sh
cargo install --path tools/semspec --locked
semspec init cases                   # creates only cases/*.md in a new project
semspec lint cases
semspec list cases
semspec run cases
semspec run cases/example.md --subject-bin /usr/bin/true
semspec review cases --strict
```

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
Timeout sends TERM, then KILL after five seconds; ordinary exit also kills remaining
group descendants. Do not detach persistent processes. Failure directories and
logs are retained, including XFAIL; `--keep` retains successful checks too.
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

The nearest common directory of the supplied directories (or file parents) is
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
just test-semspec
just semspec lint docs/src/zh/cases
just semspec run docs/src/zh/cases/01-first-job.md --subject-bin target/release/pvisor
just cases --suite use               # builds product; semspec requires every selected case to PASS
just semspec run docs/src/zh/cases --domain USE --require-pass --subject-bin target/release/pvisor
just semspec review docs/src/zh/cases --strict         # fails until human review
```

STAGE checks live in docs/src/zh/cases/06-stage-apply.md. DOC and VM appendices
use the same Markdown format and preparation from reference/index.md:

```sh
just semspec lint docs/src/zh/reference
just semspec lint tools/semspec/semantics
```

Self-specifications in semantics/review.md are drafts. AI must not execute
S-REVIEW-004. Conventional tests cover Markdown inputs, parsing, digest golden
values, review transitions, verdicts, timeout cleanup and exact content/tree helpers.

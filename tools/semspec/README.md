# semspec

A standalone Rust CLI for human-reviewed, black-box semantic preservation tests.
See [the design](DESIGN.md) and the reviewable [engine contract](ENGINE.md).
It has no pVisor dependency. The core, runner and CLI are modules in one package.
Requires Rust 1.89+, Unix and Bash. A specification is trusted executable code,
not a security sandbox; passing checks do not establish complete semantic coverage.

```sh
cargo install --path tools/semspec --locked
semspec init                         # in a new project
semspec lint
semspec list
semspec show S-EXAMPLE-001
semspec run --format json --output report.json
semspec run semantics/example.md     # select a Markdown file in spec_dirs
semspec review --strict
```

Each Markdown case combines a claim, a violation example and exactly one Bash
check. Its SHA-256 digest includes the entire normalized case, its vocabulary
and the engine version. A successful check remains UNREVIEWED until a person
reviews the prose, the check, the vocabulary and engine implementation.

Only a human may run these commands from a terminal:

```sh
semspec approve @engine @vocab:core.sh S-EXAMPLE-001 --reviewer YOUR_NAME
```

Approval displays current text/dependencies, requires typing the item,
and atomically writes only the ledger. Use Git for review diffs and history. Changing
normalized case/vocabulary bytes invalidates case approval. An engine semantic
change requires incrementing ENGINE_SEMANTICS. v0.1 relies on human review and
repository permissions; TTY checks alone cannot authenticate a reviewer.
Protect ledgers, config, vocabulary and engine with repository review
rules and a human CODEOWNER before making reviewed results a release gate.

`run [FILE.md]` supports Markdown file/case/domain selection, subject override, Bash exit-77 skips,
timeouts, retained failure directories (including XFAIL), human/JSON reports and reviewed gating.
PASS/SKIP/XFAIL exit 0; FAIL/XPASS or required pending review exit 1;
usage/config/spec errors exit 2; ERROR verdicts exit 3. Each check has a fresh
workspace and process group. Timeout sends TERM, then KILL after five seconds;
ordinary exit also clears remaining group descendants. Do not detach processes.
Checks and vocab run using the same normalized bytes that are hashed. Vocabulary
is sourced in filename order, so configuration reordering cannot silently change
execution while preserving the digest.

SSH signing, JUnit and parallel execution belong to v0.2 and are explicitly
rejected by this version. Default Bash environment is inherited; use config
`subject.env` for project settings. Runner-owned variables cannot be configured.
Write environment prerequisites in Bash and exit 77 when unavailable. SKIP takes
precedence over xfail; timeouts remain failures. Syntax lint uses bash -n. Checks
may source other files; those files must be included in project review. There is
no probe configuration, retired list, revoke command or approved snapshot store.

From the pVisor repository:

```sh
just test-semspec
just semspec lint
just semspec review --strict         # intentionally fails until human review
just semantics --case S-STAGE-001
just cases --case S-DOC-001,S-DOC-012 # documented DOC scenarios
just semantics --require-reviewed --format json --output target/semantics.json
just semspec --config tools/semspec/semantics/semspec.toml lint
```

Conventional tests cover digest golden values, parsing, ledger state transitions,
CLI verdict/review handling, exact content/tree helpers and process cleanup.
The self-specifications in `semantics/review.md` are drafts for human review.
AI must not execute S-REVIEW-004 or approve, change real ledgers/snapshots,
or weaken existing claims/checks to fit the implementation.

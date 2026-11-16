# Testing and Scrut

Unit and integration tests check implementation behavior. Semantic
specifications state product promises and require human review.

## Common commands

| Command | Purpose |
| --- | --- |
| `mise run test` | Default Rust and tooling checks, including API doctests |
| `mise run test -- pvisor-core` | One Rust package through nextest |
| `mise run test-py-unit` | Python logic, format and mocked-operation tests |
| `mise run test-py-regression` | Python subprocess, toolchain and integration regressions |
| `mise run test-py -- -k NAME` | Filter Python tests through native unittest |
| `mise run test-scripts` | Task and shell regressions |
| `mise run test-benchmark-tools` | Benchmark tooling correctness without performance measurements |
| `mise run test-doc` | Native Cargo API doctests |
| `mise run test-isolation` | Strict Linux rootless/FUSE acceptance |
| `mise run test-macos-fuse` | Explicit macOS mount acceptance |
| `mise run examples -- SCENARIO` | End-to-end product example |
| `mise run cases -- --suite stage` | STAGE semantic specifications |
| `mise run test-spec-fixtures` | Native Scrut parsing, preservation, isolation and exceptional cleanup |

## Test execution and ownership

[Test architecture](https://github.com/DeepLink-org/pvisor/blob/main/tests/README.md)
defines the principles, directory responsibilities, runners and capability gates.
Mise drives tasks; nextest, unittest, Node, Bats, Go and Scrut discover tests
and report results.

Rust private tests stay with implementation modules. Feature-related public
specifications and regressions live in `tests/spec/<feature>/`, discovered
directly through the original Cargo targets or their native runners. Other
crate contracts remain in each crate's `tests/`. Cargo target kind does not imply
that a test is free of process or mount effects. Python uses `tests/unit/` for
isolated operations and `tests/regression/` for real process boundaries.
Native runner arguments select modules and cases:

```bash
mise run test -- pvisor-cli -- --test apply_cli
mise run test-py -- tests.unit.test_docs
mise run test-py-regression -- -k stage
```

Hardware acceptance requires a capable host and matching artifacts.
Compilation, mocks, skips and ignored tests do not establish real VM or mount
coverage. See [platform requirements](../reference/platforms.md).

## Semantic specifications

Each native Scrut Markdown case has an ID, semantic claim, violation example
and executable script, with one source in `tests/spec/<feature>/S-ID.md`.
STAGE cases linked from `docs/src/zh/cases/06-stage-apply.md` cover
[staging/apply/drop promises](../concepts/staging.md); DOC cases in
`docs/src/zh/reference/cases.md` check executable documentation.

The product fixture preserves diagnostics such as PASS, FAIL, SKIP, XFAIL and
XPASS, and explicitly fails on timeouts, signals and missing prerequisites.
Native Scrut results remain separate from human semantic approval; Scrut does
not recalculate historical review states.

## Human review rules

- PASS means the implementation satisfies the script; it does not approve the claim.
- New or modified cases require human review.
- Only maintainers may approve/revoke specifications or edit `REVIEWED.toml` and `.approved/`.
- Never weaken claims, checks or xfail annotations to obtain a pass.

The specifications still await human review. Scrut has no native digest
review mechanism; maintainers have not yet explicitly decided its replacement.
`--require-reviewed` therefore refuses execution rather than bypassing the gate.
Current rules, layout and commands are in the
[specification README](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/README.md).

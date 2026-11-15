# Testing and semspec

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
| `mise run test-semspec` | The semspec implementation |
| `mise run semspec -- lint docs/src/zh/cases` | Static specification checks |

## Test execution and ownership

[Test architecture](https://github.com/DeepLink-org/pvisor/blob/main/tests/README.md)
defines the principles, directory responsibilities, runners and capability gates.
Mise drives tasks; nextest, unittest, Node, Bats, Go and semspec discover tests
and report results.

Rust private tests stay with implementation modules; external contracts and
integrations live in each crate's `tests/`. Cargo target kind does not imply
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

Each Markdown case has an ID, semantic claim, violation example and executable
script. STAGE cases in `docs/src/zh/cases/06-stage-apply.md` cover
[staging/apply/drop promises](../concepts/staging.md); DOC cases in
`docs/src/zh/reference/cases.md` check executable documentation.

Execution results are PASS, FAIL, SKIP, XFAIL, XPASS or ERROR. Review states
are UNREVIEWED, STALE or REVIEWED.

## Human review rules

- PASS means the implementation satisfies the script; it does not approve the claim.
- New or modified cases require human review.
- Only maintainers may run `semspec approve`/`revoke` or edit `REVIEWED.toml` and `.approved/`.
- Never weaken claims, checks or xfail annotations to obtain a pass.

See `tools/semspec/DESIGN.md` for the specification and review model.

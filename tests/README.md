# Host regression tests

Run `just test` for Rust, native script regressions and Python tests. A Cargo
package argument selects only Rust tests. `just test-py` runs Python tests;
`just test-scripts` runs the private recipes imported from `tests/justfile`.
Python tests use standard-library `unittest`, without a pytest dependency or
configuration. CI runs both entries explicitly. Delete superseded Python wrappers
after moving their behavioral checks; do not retain a second set of recipe or shell-source
string assertions.

Keep Just routing, Bash vocabulary, firmware Make and shell installer checks in
`tests/justfile`. Test routing branches, failure handling and publication behavior;
do not repeat each one-line recipe as a mock command assertion.
Python tests cover Python contracts, structured data, PTY interaction and process
lifecycle. Do not launch Just or embed Bash checks inside a Python test runner.
Calling a product command or supervising an example process remains part of
the integration tests.

Use `tests.fixtures.workspace()` for an isolated canonical temporary path and an
`ExitStack` for mocks and other cleanup. Each use owns its resources; cleanup runs
before removing the directory, including when an assertion fails.

Semantic specifications live in user Markdown under `docs/src/zh/cases/` and
`docs/src/zh/reference/`, following `tools/semspec/DESIGN.md`. Moving conventional regression checks does not approve
specifications or change their claims, vocabulary, review ledger or snapshots.

## Docker CLI compatibility

`podman/` retains pinned upstream Podman system/e2e sources and adapted black-box
Bats/Go contracts that run the top-level pVisor container commands and check expected behavior. Docker
daemon interoperability cases are disabled with reasons. See
[podman/README.md](podman/README.md) for explicit runtime commands, prerequisites
and coverage limits. The external-runtime suites are opt-in locally and required
in the Linux CLI CI shard; normal Python regressions verify source integrity and
adapter behavior without a daemon.

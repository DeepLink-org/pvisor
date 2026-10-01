# pVisor Agent Instructions

## Default project scope

The active product is pVisor: the executor, Gateway, Core, OverlayFS,
OverlayNet, replay, and the `pvisor-core` records those components share.

Prefer targeted build, lint, and test commands over workspace-wide commands
when a workspace-wide command would pull unrelated crates into scope.

## Test command

Use `just test` as the default validation command. It runs Rust tests through
`cargo nextest` in debug mode (for faster iteration) and then runs the Python
test suite. To limit Rust coverage to one package, pass its Cargo package name
positionally, for example `just test pvisor-core`. Short aliases are
`pvisor`, `core` (`control` and `agentctl` remain aliases), and `capture` (Gateway). Use a direct `cargo test`
invocation only when doctests or an explicitly documented special runner is
required.

## Semantic preservation specifications

Follow `tools/semspec/DESIGN.md` for semspec. AI may draft new cases, repair the
subject implementation and maintain conventional runner tests. AI must not run
`semspec approve`/`revoke`, edit real `REVIEWED.toml` ledgers or `.approved/`
snapshots, or weaken existing claims/checks/xfail annotations to obtain PASS.
Use `just test-semspec` to validate the standalone runner and `just semspec lint`
for specifications. Human approval is distinct from a passing test.

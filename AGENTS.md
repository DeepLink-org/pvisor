# pVisor Agent Instructions

## Default project scope

The active product is pVisor: the executor, Gateway, Core, OverlayFS,
OverlayNet, replay, and the `pvisor-core` records those components share.

Prefer targeted build, lint, and test commands over workspace-wide commands
when a workspace-wide command would pull unrelated crates into scope.

## Directory READMEs

Before changing any file, read the `README.md` of every directory on the path
from the repository root to that file. A directory README's rules apply to
everything below it and are not optional context.

## Crate API boundaries

Migrate one crate at a time, following `pvisor-vm`'s API model. Do not turn an
incremental boundary change into a workspace-wide rewrite. Each migrated crate
must satisfy these rules:

- `lib.rs` exposes only `pub mod api`; implementation modules remain private.
  Do not retain root-level compatibility exports or expose an internal module
  to work around the boundary.
- `api` owns public data contracts and trait method declarations. Opaque owners
  may be re-exported from private implementations, but their state stays private.
  Method bodies, validation, platform dispatch and resource management belong
  in private implementations. Do not add public inherent methods there or put
  default method bodies in API traits.
- Keep API declarations uniform across the crate's supported platforms and
  features. Document capabilities and explicit unsupported-operation errors;
  keep platform conditionals in implementations rather than public signatures.
- Cross-crate callers use the owning crate's `api`, not its implementation state.
  Internal module collaboration should pass explicit inputs and ownership rather
  than sharing mutable implementation details. Avoid equivalent duplicate DTOs,
  blanket re-exports and wrappers that merely relocate the old public surface.
- Document every public contract, including fields and methods. Explain input
  validation, ownership, lifecycle/call ordering, synchronization, side effects,
  unsupported cases and state after failure where applicable. Enforce missing
  API documentation and check rustdoc with warnings denied.
- Test contracts from outside the crate through `api`: construction/defaults,
  validation, errors and ownership/lifecycle behavior. Keep private-state tests
  inside the crate. Add boundary guards against implementation exports, API
  bodies/conditional declarations and undeclared public methods. Do not expose
  internals solely for tests or weaken existing coverage during migration.
- Update affected callers and the crate README in the same step. Validate the
  selected crate and compile affected consumers; report environmental/platform
  coverage gaps separately from passing contract tests.

Crates not yet migrated retain their current interfaces until their own focused
refactor. `pvisor-vm` and `pvisor-overlayfs` currently follow this boundary.

## Benchmarks

Any work under `benchmark/`, `docs/src/*/benchmarks/` or
`docs/src/assets/benchmarks/` must follow `benchmark/README.md`: find the
benchmark's registry entry before measuring or writing, keep each user page in
the order conclusions, motivation, experiment design, data and analysis, and
keep engineering A/B and diagnostic results out of user pages.

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

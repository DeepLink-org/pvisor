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

## Comment discipline

- Keep comments short and factual. Explain only non-obvious constraints or
  reasons the code cannot express; do not repeat code or narrate edits.
- Never use comments to excuse workarounds, deferred fixes or unnecessary
  complexity. Fix the cause; any retained limitation must state its concrete
  effect and removal condition, without defending it on effort or scope grounds.
- Preserve required API documentation and necessary safety, ownership,
  synchronization and platform preconditions. Brevity must not omit a contract.
- Prefer code, types or tests for enforceable rules. Remove stale or redundant
  comments when touching the relevant code.

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
refactor. `pvisor-vm`, `pvisor-overlayfs`, `pvisor-journal`,
`pvisor-containers`, `pvisor-replay`, `pvisor-overlaynet`, `pvisor-guest`,
`pvisor-daemon`, `pvisor-shim`, and `pvisor-cli` follow this boundary.

## Benchmarks

Any work under `benchmark/`, `docs/src/*/benchmarks/` or
`docs/src/assets/benchmarks/` must follow `benchmark/REGISTRY.md`: find the
benchmark's registry entry before measuring or writing, keep each user page in
the order Motivation, experiment design, data and analysis (three sections), and
keep engineering A/B and diagnostic results out of user pages.

## Test command

Use `mise run test` as the default validation command. It runs Rust tests through
`cargo nextest` in debug mode (for faster iteration) and then runs the Python
test suite. To limit Rust coverage to one package, pass its Cargo package name
positionally, for example `mise run test -- pvisor-core`. Short aliases are
`pvisor`, `core` (`control` and `agentctl` remain aliases), and `capture` (Gateway). Use a direct `cargo test`
invocation only when doctests or an explicitly documented special runner is
required.

## Semantic preservation specifications

AI may draft new cases, repair the subject implementation and maintain
conventional fixture tests. AI must not approve/revoke specifications, edit real
`REVIEWED.toml` ledgers or `.approved/` snapshots, or weaken existing
claims/checks/xfail annotations to obtain PASS.
Product specifications live in `tests/spec/<feature>/` and run through native
Scrut using `mise run cases`. Use `mise run test-spec-fixtures` for their
preservation, isolation and failure contracts. Human approval remains
distinct from a passing test; the Scrut digest-review replacement is pending
an explicit maintainer decision and `--require-reviewed` fails closed.

# Persisting Agent Instructions

## Default project scope

The active product is pVisor: the executor, Gateway, Control, OverlayFS,
OverlayNet, replay, and the `persisting-control` records those components share.

Prefer targeted build, lint, and test commands over workspace-wide commands
when a workspace-wide command would pull unrelated crates into scope.

## Test command

Use `just test` as the default validation command. It runs Rust tests through
`cargo nextest` in debug mode (for faster iteration) and then runs the Python
test suite. To limit Rust coverage to one package, pass its Cargo package name
positionally, for example `just test persisting-control`. Short aliases are
`pvisor`, `control` (`agentctl` remains an alias), and `capture` (Gateway). Use a direct `cargo test`
invocation only when doctests or an explicitly documented special runner is
required.

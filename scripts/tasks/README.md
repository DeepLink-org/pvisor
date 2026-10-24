# Task implementations

These Bash scripts implement the build, case routing, local CI, Rust test and
wheel tasks declared in the root `mise.toml`. Invoke them with `mise run` so they
receive the repository working directory, pinned tools and shared environment.
Task names and visibility belong in TOML; argument parsing, signing and
publication behavior belong in the scripts. Keep small commands inline.

Validate routing and failure behavior with `mise run test-task-routing`.

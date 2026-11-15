# Task implementations

These scripts implement the tasks declared in `mise.toml`. Invoke them through
`mise run` for the project directory, pinned tools and shared environment.

Task names and dependencies belong in TOML. Scripts own argument handling,
signing and artifact publication. Native runners own test discovery and results.
Keep small commands inline.

Run `mise run test-task-routing` to check routing and failure behavior.
See [test architecture](../../tests/README.md) for suite ownership.

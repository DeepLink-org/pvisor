# pVisor application frontends

`pvisor-cli` owns the `pvisor`, `pvisor-cache`, `pvisor-memory-pool`,
`pvisor-tui`, and `pvisor-replay` executables and their argument parsing, terminal
adapters, rendering, companion lookup and local resource-owner supervision.
The TUI frontend lives here, not in the embeddable runtime.

## Boundary

Embed [`pvisor`](../pvisor/README.md) for `PVisor`, Session/Attempt execution and
`job_service::RuntimeJobService`. This application library exposes frontend
modules, not a compatibility re-export of the runtime. Frontends call explicit
runtime APIs and retain request-local cancellation, admission fences and terminal
ownership. Neither the persistent Host Job listener nor the local node/pool
supervisor is an implicit requirement of embedded execution.

Recording callers import `Journal` and `JournalStore` from `pvisor_journal::api`;
durable stage preparation uses its `Persistence` and `DurableFiles` contracts.

## Source layout

- `src/cli/`: Job commands, typed internal Host requests/listener/workers,
  safe Agent presets, terminal adapters and output rendering.
- `src/cli/features.rs`: runtime feature listing frontend; registry/settings
  remain in `pvisor::features`.
- `src/cli/cache.rs`: cache command parsing and rendering; cache storage,
  image preparation and authenticated server implementation remain in `pvisor`.
- `src/companions.rs`: trusted same-installation companion discovery/dispatch.
- `src/service.rs` and `src/service_cgroup.rs`: local resource-owner process
  supervision and delegated cgroup resolution, not the runtime Job service.
- `src/tui/`: native TUI PTY runtime, renderer, review panels and keymap,
  including Zellij attribution and the adapted border-glyph license.
- `src/bin/pvisor-replay.rs`: replay argument parsing and managed-run frontend;
  the independent replay engine remains in `pvisor-replay`.
- `src/bin/`: executable entry points.
- `tests/`: executable/frontend integration tests, including tests that combine
  runtime APIs with command execution. Runtime-only tests remain in `pvisor`.

The runtime README retains detailed execution, control, storage, service and
platform limits. Its command examples refer to these installed frontends.

## Build and validation

`gateway` forwards to `pvisor/gateway`; default builds keep Gateway optional.
Use `just test pvisor-cli` for application tests and `just test pvisor` for
runtime tests. Native VM and ignored environment-dependent gates retain their
existing prerequisites; compile checks are not real-guest validation.

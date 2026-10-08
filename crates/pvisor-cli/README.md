# pVisor application frontends

`pvisor-cli` owns four executables: `pvisor`, `pvisor-cache`, `pvisor-tui`, and
`pvisor-replay`, with their argument parsing, terminal adapters, rendering and
companion lookup.
The TUI frontend lives here, not in the embeddable runtime.

## Boundary

Embed [`pvisor`](../pvisor/README.md) for `PVisor`, Session/Attempt execution and
`job_service::RuntimeJobService`. This application library exposes frontend
modules, not a compatibility re-export of the runtime. Frontends call explicit
runtime APIs and retain request-local cancellation, admission fences and terminal
ownership. The persistent Host Job listener is not an implicit requirement of
embedded execution.

The separate `pvisor-daemon` owns shared services, including the memory pool.
Invoke `pvisor-daemon` and `pvisor-cache` directly. Replay and TUI are root
companions. Use `pvisor -- COMMAND` for explicit workload execution.

Recording callers import `Journal` and `JournalStore` from `pvisor_journal::api`;
durable stage preparation uses its `Persistence` and `DurableFiles` contracts.

## Command help and live VM controls

Root help stays concise: **Execution** (`run`, `status`, `kill`), **Changes**
(`review`, `apply`, `drop`, `inspect`), **Checkpoints** (`checkpoint`, `suspend`,
`resume`, `fork`) and **Tools** (installed `replay`/`tui` companions, `feature`,
`help`). It includes the run-review-apply examples; use `pvisor help COMMAND`
for command options.

```bash
pvisor run --safe -- claude
pvisor review last
pvisor apply last --path src
```

The seven live VM flags are command-local, under **Live VM** in command help:

| Command | Accepted live VM flags |
| --- | --- |
| `status` | `--vm-socket`, `--vm-job-id`, `--vm-attempt-id` |
| `suspend` | The three identity flags plus `--vm-pause`, `--vm-offload`, `--vm-ram-file` |
| `resume` | The three identity flags plus `--vm-load` |

Place these flags after the supported command. Root-prefixed live VM flags and
live VM flags on other commands are rejected. All three identity flags are
required together; suspend/resume's positional Job must match `--vm-job-id`.
Pause and offload are mutually exclusive; `--vm-ram-file` requires offload.
Live resume continues the same Attempt. Ordinary persisted-Job suspend/resume
retain execution checkpoint capture/restoration. See the runtime README and
[CLI reference](../../docs/src/en/reference/cli.md#vm-instance-control) for
examples and endpoint/lifecycle limits.

Help, version and feature queries emit no startup logs. For execution commands,
`process.entry` and `cli.parsed` are emitted only after command parsing; these
markers do not measure the complete process-entry or argument-parsing overhead.

## Source layout

- `src/cli/`: Job commands, typed internal Host requests/listener/workers,
  safe Agent presets, terminal adapters and output rendering.
- `src/cli/features.rs`: runtime feature listing frontend; registry/settings
  remain in `pvisor::features`.
- `src/cli/cache.rs`: cache command parsing and rendering; cache storage,
  image preparation and authenticated server implementation remain in `pvisor`.
- `src/companions.rs`: trusted same-installation companion discovery/dispatch.
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

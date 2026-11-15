# Docker Compose regressions

Executable regressions and feature-local fixtures live here. Follow the
[feature instructions](../README.md) and [specification](../spec.md).

`compose_cli.rs` is the explicit Cargo source for the `compose_cli` target.
The acceptance, chaos and edge-case modules remain selected through
`docker_cli`, preserving their native fixture and test names.

```sh
mise run test -- pvisor-cli -- --test compose_cli --test docker_cli
```

For explicitly selected real-VM chaos coverage, use the prerequisites and
commands in [the Compose runbook](../../../COMPOSE_CHAOS.md). Default ignored
hardware cases are unexecuted coverage, not successful acceptance results.

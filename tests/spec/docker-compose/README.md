# Docker Compose profile specifications

Read [spec.md](spec.md) for the feature contract. This feature owns project and
multi-service orchestration, separately from individual
[Docker basic commands](../docker-cli/spec.md).

## Regression tests

The `compose_cli` integration source and native acceptance, chaos and edge-case
modules now live in [regression/](regression/README.md). Cargo retains the
`compose_cli` target; native Compose modules retain their original `docker_cli`
selection names and shared runtime fixture. Parser checks do not replace
runtime checks.

See [the existing Compose instructions](../../COMPOSE_CHAOS.md) for exact
selection commands, fixture preparation, fault oracles and evidence handling.
The native checks require Linux/KVM or Apple Silicon/HVF with matching firmware
and a prepared image. Ordinary schedule/watchdog tests do not establish that
the real VM workflows pass.

## Native Scrut contracts

[Config export](S-COMPOSE-001.md) and [admission failure](S-COMPOSE-002.md)
exercise the real frontend without a VM, registry or Docker Compose executable.
They own their input files, stores and Session directory. These new IDs are
separate from migrated DOC/USE/STAGE cases and from guest lifecycle acceptance.

```sh
mise run cases -- --feature docker-compose --list
mise run cases -- --feature docker-compose
```

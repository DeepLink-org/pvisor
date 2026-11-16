# Docker basic command specifications

Read [spec.md](spec.md) for the feature contract. This feature owns individual
container and image commands; multi-service project behavior belongs to
[docker-compose](../docker-compose/spec.md).

## Regression tests

The active Rust integration targets, Bats `system/`, Go `e2e/` and mocked
adapter regressions now live in [regression/](regression/README.md). Cargo
preserves the original target names through explicit source paths. CI and
fixture paths use the relocated sources; vendored upstream references remain
under `tests/podman/upstream/`.

See [the runner instructions](../../podman/README.md) for Bats and Go
commands, private-store ownership, image preparation, firmware and Linux/KVM or
Apple Silicon/HVF prerequisites. These tests need a real runtime; parser or
mock tests do not establish container execution.

## Native Scrut contracts

The new [offline image lifecycle](S-DOCKER-001.md) and
[inert command refusal](S-DOCKER-002.md) cases use the real executable, private
stores and independent filesystem oracles. They require Bash and Python 3, not
a VM, FUSE or a registry. They do not replace the VM runtime regressions.

```sh
mise run cases -- --feature docker-cli --list
mise run cases -- --feature docker-cli
```

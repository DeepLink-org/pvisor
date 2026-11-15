# Docker basic command regressions

Executable regressions and feature-local fixtures live here. Follow the
[feature instructions](../README.md) and [specification](../spec.md).

Cargo targets retain their names: `docker_cli`, `docker_p0_cli`,
`docker_staged_cli` and `docker_staged_native`. Run from the repository root:

```sh
mise run test -- pvisor-cli -- --test docker_cli --test docker_p0_cli --test docker_staged_cli --test docker_staged_native
PVISOR_BINARY="$PWD/target/debug/pvisor" bats tests/spec/docker-cli/regression/system
PVISOR_BINARY="$PWD/target/debug/pvisor" go test -v tests/spec/docker-cli/regression/e2e/*.go
```

See [runtime preparation and coverage limits](../../../podman/README.md)
before running native cases. Ignored hardware cases are not passing coverage.
`test_podman_contracts.py` checks the adapter with mocks and is included in
default Python discovery.

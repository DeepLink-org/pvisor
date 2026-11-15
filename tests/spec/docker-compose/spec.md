# Docker Compose profile

## Feature boundary

This feature specifies pVisor's supported `pvisor compose` profile. It does not
invoke Docker Compose and does not promise complete Compose-spec compatibility.
Individual container and image commands belong to the separate
[Docker basic commands](../docker-cli/spec.md) feature.

## Required behavior

The existing [native Compose contracts](../../COMPOSE_CHAOS.md) define concrete
fault scenarios and independent oracles. Preserve these requirements when
moving their regressions:

- Stop/start retains container identity and writable state while creating the
  documented new launch generation.
- Configuration reconciliation honors `--no-recreate`; ordinary reconciliation
  replaces only the changed service and applies its new configuration.
- Invalid bind admission reports an error without destroying the existing
  workload or its writable state.
- Workload failures retain their exact exit status; subsequent orchestration
  follows the documented restart and state-retention behavior.
- Concurrent clients do not create duplicate or abandoned project members.
- Project teardown and recreation preserve named-volume data unless explicitly
  requested otherwise, without affecting a foreign project in the same store.
- Frontend failure must not strand a project lease or prevent the documented
  dependency recovery.

## Isolation and coverage

Native regressions own their private store, projects, volumes and processes.
They bound readiness waits and command execution, verify guest-observed data,
and preserve failure diagnostics and cleanup errors.

Use real VM workflows for lifecycle and recovery guarantees. Mocks, schedule
tests and successful YAML parsing are narrower evidence. Missing prerequisites
must fail an explicitly selected acceptance gate.

This document groups the feature's existing promises. Executable sources live
in [regression/](regression/README.md). This contract does not replace their
individual assertions, authorize weaker exception handling or imply that
runtime validation has passed on every supported platform.

## Frontend admission and configuration

| Operation | Contract and failure state | Executable evidence |
| --- | --- | --- |
| `config`, `config -q`, `config --services` | Explicit project naming wins; services are deterministic; normalized export preserves literal dollars on reload; input files and stores remain unchanged | [S-COMPOSE-001](S-COMPOSE-001.md) and `compose_cli` config regressions |
| Dependency and privilege admission | Missing/cyclic dependencies, unknown selected services and unsupported privilege are rejected before storage or guest launch | [S-COMPOSE-002](S-COMPOSE-002.md) |
| `env_file` resolution | Relative files resolve from the Compose file's directory, later files override earlier files, explicit environment overrides files, and an explicit empty value stays empty; normalized export needs no original env files | `compose_cli::compose_env_files_resolve_from_config_directory_and_failure_preserves_inputs` |
| Missing `env_file` on `up -d` | Nonzero exit and missing-file diagnostic; no stores or Sessions are created and input config/remaining env files are unchanged | Same native frontend regression |
| `build` | Local build metadata is inspectable without creating service containers or named volumes; invalid rebuild preserves the previous image | `compose_cli::compose_build_uses_local_context_without_creating_service_containers` |

These frontend contracts need the real CLI and local files but no VM or registry.
The native acceptance/chaos modules remain the evidence owners for running
project reconciliation, dependency recovery, leases and volume retention. The
frontend checks do not establish those guest lifecycle guarantees.

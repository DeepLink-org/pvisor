# Feature specifications

Organize product specifications by user-visible feature. Each feature directory
owns its Markdown contract and the regression tests that exercise that contract.
Runner or historical suite names are not feature boundaries.

```text
tests/spec/
  docker-cli/
    README.md
    spec.md
    regression/
  docker-compose/
    README.md
    spec.md
    regression/
```

## Directory contract

- `README.md` describes the feature boundary, prerequisites and native test
  commands. State explicitly which checks need a real VM or other hardware.
- `spec.md` describes the supported behavior, errors, observable state after
  failure and known limits. Keep claims distinct from verified coverage.
- Additional Markdown files may hold independently selectable Scrut cases.
  Preserve stable case IDs when moving an existing specification.
- `regression/` contains the feature's executable regression sources and local
  fixtures. Use the native runner; do not add a feature-specific test dispatcher
  or a second Markdown parser.
- Keep one authoritative copy of each specification and test. Documentation
  should link to that source instead of maintaining a second executable copy.

Private-state Rust tests remain inside their owning crate. Cargo integration
targets may reference Rust regression modules here explicitly; moving a source
must not silently remove it from Cargo discovery. Shared resource helpers and
vendored reference suites retain their own ownership rather than being copied
into every feature.

## Features

| Feature | Contract | Scope |
| --- | --- | --- |
| Docker basic commands | [docker-cli/spec.md](docker-cli/spec.md) | Individual container and image commands |
| Docker Compose profile | [docker-compose/spec.md](docker-compose/spec.md) | Project and multi-service orchestration |
| Sessions | [session/README.md](session/README.md) | Launch, completion, status and cancellation |
| Staging | [stage/README.md](stage/README.md) | Review, selective apply, conflicts, drop and staged binds |
| Checkpoints | [checkpoint/README.md](checkpoint/README.md) | Checkpoint, restore and fork |
| Configuration | [config/README.md](config/README.md) | TOML/RunSpec input, result publication and parse refusal |
| Containers | [container/README.md](container/README.md) | Native OCI Session executor, distinct from Docker commands |
| Gateway | [gateway/README.md](gateway/README.md) | Gateway routing and capture |
| Isolation | [isolation/README.md](isolation/README.md) | Host boundary and denied effects |
| Network | [network/README.md](network/README.md) | Network configuration and policy |
| Replay | [replay/README.md](replay/README.md) | Recorded execution and replay |
| Resources | [resources/README.md](resources/README.md) | Resource limits and timeout |
| VM | [vm/README.md](vm/README.md) | VM Sessions, machine control and restore |

## Execution and evidence

The 88 migrated product cases live in feature-local `S-ID.md` files: 16 USE,
14 STAGE and 58 DOC (including six VM-control cases). Scrut 0.4.3 owns Markdown
parsing and execution; resource fixtures provide independent workspaces and
bounded process cleanup. The report gate checks the exact selected inventory.

New contracts use separate ID domains (`S-DOCKER-*`, `S-COMPOSE-*`,
`S-CONFIG-*`) and state their origin in each file. They do not replace or
renumber migrated cases. `--feature` discovers every `S-*.md` in that feature;
historical `--suite` domains do not silently grow to include these prefixes.
Native Scrut also discovers the files directly, without a maintained case list.

```sh
mise run cases -- --suite use --list
mise run cases -- --feature stage --list
mise run cases -- --feature stage
mise run cases -- --feature docker-cli
mise run cases -- --feature docker-compose
mise run test-spec-fixtures
```

Docker and Compose conventional regressions have also moved into their feature
directories. Their READMEs describe native Cargo, Bats and Go entry points.
Private-state crate tests and vendored upstream references retain their owners.

Historical IDs remain selectable with `--case`; suite selectors retain their
previous domains. `--require-reviewed` refuses execution until maintainers
authorize an equivalent review policy. Existing ledgers are not approvals for
the migrated Scrut engine and are not rewritten by tests.

Preserve existing assertions, exception reasons and approval records. Passing
tests are execution evidence, not human approval. Missing prerequisites in an
explicit acceptance gate must fail rather than produce a misleading success.

## Scenario resource ownership

`fixture.py` assigns a fresh workspace, HOME, XDG directories, Session directory,
Docker store and image store per case, overriding inherited explicit store paths.
Image inputs needed for VM acceptance must be prepared for the owned store;
pointing an ambient store at someone else's data is not an isolation strategy.
Stores start absent so config/validation cases can detect unwanted initialization.
Success normally deletes scenario resources; failure, XFAIL and `--keep` retain
inputs, exact output streams and verdict metadata. Retaining evidence does not
leave a workload intentionally running.

On Linux the fixture is a child subreaper: it first terminates its process group,
then kills and reaps its adopted descendants, including setsid/double-fork
children. It never kills by command name or scans unrelated processes. Cleanup
is bounded; inability to release owned children is a gate failure. On macOS
cleanup covers the owned process group only: cases must not detach or
daemonize children there. Such detached lifecycle acceptance requires a native
resource owner rather than a claim that the generic fixture cleans it up.

Both Linux and macOS observe the scenario anchor with `waitid(WNOWAIT)` and
retain its identity until process-group signals finish. macOS uses the native
libc interface because Python does not expose `os.waitid` there. Darwin's
zombie-only `killpg` EPERM is accepted only when the direct child is still
waitable and exited, and a bounded query of that exact group contains the anchor
and exclusively zombie members. A live member, missing anchor, lost ownership,
failed query or genuine permission error fails cleanup; these are not skipped
tests. Mocked ABI/error regressions supplement, not replace, native macOS CI.

An explicit prerequisite refusal (`exit 77`) is always a failed Scrut gate, even
on an XFAIL platform. Unexpected signals, missing commands and timeouts are not
accepted XFAIL outcomes. `PVISOR_CASE_REQUIRE_PASS=1` also rejects XFAIL; USE IDs
always require PASS, including when selected through a feature rather than the
historical USE suite.

# Docker basic commands

## Feature boundary

pVisor provides a Docker-style CLI profile for individual containers and images.
The supported command profile is not a claim of complete Docker compatibility
or implementation of the Docker Engine API.

The public [CLI reference](../../../docs/src/en/reference/cli.md) defines the
supported commands and options. This specification groups their observable
contracts and associated regressions; it does not expand that profile.

## Required behavior

- Supported container operations must preserve their documented exit status,
  output and lifecycle state. Verify `run`, `exec`, inspection, logs and
  stop/remove behavior through the public CLI.
- Filesystem, environment, identity and network options must produce their
  documented effects, not merely parse successfully.
- Image operations and local builds must validate their resulting image or
  runnable contents, not only the command's success message.
- Unsupported commands or options must fail explicitly rather than executing
  an unintended host workload.
- A failed operation must preserve the state promised by its command contract.
  Each regression must check the relevant post-failure state.

## Isolation and coverage

Each runtime regression owns its store, containers, input files and processes.
Waits must be bounded and cleanup must target only test-owned resources.
Explicit runtime gates fail when virtualization, firmware or image preparation
is unavailable.

Existing coverage and its limits are recorded in the
[Docker CLI test instructions](../../podman/README.md). In particular, these
tests do not establish full upstream compatibility, cross-VM mmap coherence or
physical deletion of backing storage.

Concrete regressions in [regression/](regression/README.md) retain their
independent assertions and original Cargo target identities. This feature-level
contract is not a substitute for those cases or evidence that they have passed.

## Concrete command contracts and oracles

| Boundary | Observable result and state after failure | Executable evidence |
| --- | --- | --- |
| `build` from a local scratch context | Inspect returns the tag's image identity and configured environment; an invalid rebuild preserves the previous image and input bytes; removing the tag makes lookup fail | [S-DOCKER-001](S-DOCKER-001.md), `docker_cli::docker_build_cli_exports_scratch_image_and_keeps_tags_on_error` |
| Command routing | Unknown root commands cannot fall back to host execution; invalid `run`, `cp` and filter arguments create neither stores nor Sessions | [S-DOCKER-002](S-DOCKER-002.md), `docker_cli` inert routing checks |
| Stopped-container `cp` | Copy preserves exact binary bytes; missing container/source, traversal and invalid destinations fail without truncating an existing host file or the prior container destination; a second container does not inherit the first one's upload | `docker_p0_cli::failed_copy_preserves_existing_destinations_and_other_container_content`, existing merged-content/link checks |
| Named volumes | Referenced volumes cannot be removed even for stopped containers; container `rm -v` retains a named volume; symlinks cannot be adopted as volumes | `docker_p0_cli::volume_commands_refuse_stopped_references_and_never_adopt_links` |
| `run` / `exec` / `stop` / `start` / `logs` / `rm` | Guest output and exit status are exact, writable state survives stop/start, and teardown owns only the test's containers | Ignored native VM lifecycle tests in `docker_cli` and `common/docker_p0_acceptance.rs`; must be explicitly executed on a prepared machine |

Offline image publication and stopped-container file operations establish their
own contracts, not guest execution or the `run`/`exec` lifecycle. An empty
default test selection for native VM cases is not evidence for those promises.

# Learn pVisor through tasks

Start with a trusted script, then stage files, inspect execution evidence, save proposals, and explore branches before setting boundaries for untrusted work. Each chapter explains when to use a capability, which command to run, and what result to expect.

| Order | Your question | Commands | Executable cases |
|---|---|---|---|
| [1. First Session](01-first-session.md) | How do I record scripts, failures, and deadlines? | `run`, `status`, `--config` | S-USE-001–004 |
| [2. Review and decide](02-review-and-decide.md) | What did the Agent change, and what should I accept? | `review`, `inspect`, `apply`, `drop`, `kill` | S-USE-005–008 |
| [3. Save and branch](03-checkpoint-and-fork.md) | How do I retain a proposal and compare independent executions? | `checkpoint create/list/show`, `fork` | S-USE-009–011 |
| [4. Set boundaries](04-boundaries.md) | How do I restrict file access and network connections? | `--safe`, `--access`, `--overlaynet-deny-all` | S-USE-012–014 |
| [5. History and restoration](05-tools-and-restoration.md) | Should I restore files, Agent history, or the whole VM? | `replay`, capability inspection | S-USE-015–016 |

After the five chapters, [check staging, apply and drop contracts](06-stage-apply.md) with 14 STAGE checks. The USE gate selects only S-USE cases from the first five chapters.

## Complete one file review

After installation, run these commands in a test directory. Use an absolute stage path outside the workspace so execution records are not treated as project files. The original directory has no report.txt after the command; inspect reads the proposal, and apply creates the file in the original directory.

```bash
sandbox=$(mktemp -d)
mkdir "$sandbox/workspace"
cd "$sandbox/workspace"
pvisor spawn --stage "$sandbox/draft" -- /bin/sh -c 'printf proposal > report.txt'
pvisor review "$sandbox/draft" --diff
pvisor inspect "$sandbox/draft" -- /bin/cat report.txt
pvisor apply "$sandbox/draft" --path report.txt
cat report.txt
```

This stages workspace files while host file access remains ambient. Before running an untrusted Agent, continue to chapter 4, inspect actual isolation evidence, and read about [execution environments](../guides/executors/index.md).

## Choosing companion tools

| Need | Tool and entry point | Next step |
|---|---|---|
| Route real model requests and capture trajectories | `pvisor spawn --gateway-mode capture --gateway-route ...` | [Capture guide](../guides/capture.md) and the local mock-request test in `mise run examples -- 04-gateway-llm-control` |
| Prepare restoration from native Agent history | `pvisor replay`, dispatched to adjacent `pvisor-replay` | [Chapter 5](05-tools-and-restoration.md), [replay guide](../guides/replay.md) |
| Select an OCI container or VM executor | `pvisor spawn --executor container` / `--executor vm` | [Containers](../guides/executors/container.md), [VMs](../guides/executors/vm.md); requires a runtime/rootfs |
| Save CPU, RAM, devices, and the complete file tree | Native VM execution checkpoints / snapshot storage SDK | [Execution-checkpoint contract](../reference/cli.md#full-vm-execution-checkpoints); requires a compatible owned-rootfs, no-network Session; not a daemon capability |
| Manage OCI file caching or a shared cold-page pool | `pvisor-daemon serve --cache` / `pvisor-daemon serve --sandbox --memory-pool` | [Shared image cache](../reference/shared-image-cache.md), [daemon pool](../guides/daemon/index.md#memory-pool) |

Gateway capture requires a build with the gateway feature; wheels and `mise run build -- release` include it. Core Session commands work without companion binaries; `pvisor --help` lists installed optional commands in their object group. Full VM snapshots require KVM or Apple Silicon Hypervisor and a working FUSE backend. Ordinary VM Sessions support execution checkpoints with a compatible owned-rootfs, no-network profile; `status --json` reports capability and blockers.

## Execute the documentation

These chapters retain the learning order and semantic descriptions. Each scenario links to its single native Scrut Markdown check in `tests/spec/<feature>/S-ID.md`. Tests are organized by feature. Scrut 0.4.3 owns discovery, execution and matching; small product fixtures own independent workspaces, strict Bash, deadlines and process cleanup. Check scripts and preparation functions have one source under `tests/spec/`.

```bash
mise run cases -- --suite use --list
mise run scrut -- test tests/spec/session/S-USE-001.md
mise run cases -- --suite use
mise run cases -- --suite use --case S-USE-005,S-USE-007 --keep
mise run cases -- --suite use --output target/pvisor-learning-report.json
```

`mise run cases -- --suite use` builds the release product and companions, then executes all 16 cases in isolated temporary workspaces, HOME, XDG and Session data directories. Linux CI checks FUSE and user/mount/network namespaces first. Missing prerequisites fail the gate rather than count as success. Native Scrut JSON goes to `target/pvisor-learning-report.json`; failures retain their workspaces and `--keep` also retains successful ones. The task selects IDs by filename and requires the native report's source-file set to match the selection exactly. Empty reports, missing or duplicate results, SKIP, XFAIL and other non-PASS outcomes cannot pass the USE gate. Valid selection clears the old report before atomically publishing fresh results. Feature selection such as `--feature stage` includes that feature's scenarios across historical domains.

These specifications and fixtures still await human semantic review; passing Scrut does not approve them. Historical ledgers remain unchanged. Scrut has no native digest-review mechanism, and maintainers have not yet decided the review policy. `--require-reviewed` therefore explicitly refuses execution rather than silently bypassing the gate. Tests do not write approval records.

[DOC cases](../reference/cases.md), [VM control cases](../reference/cases-vm.md), STAGE and USE scenarios run through `mise run cases`, selected with `--suite doc/stage/use/vm`; `mise run examples` runs examples/pvisor. CI runs existing isolation regressions and network/Gateway mock scenarios alongside this learning path. Retired standalone snapshot hardware records remain historical evidence. Legacy Controller/Worker acceptance records are not validation of the new daemon. Validate native execution restoration separately against the [execution-checkpoint contract](../reference/cli.md#full-vm-execution-checkpoints); the daemon's [runtime boundaries](../guides/daemon/index.md) do not include VM restore. This learning path does not count unexecuted VM/Gateway capabilities as success.

The full execution gate currently targets Linux. On macOS, select applicable cases individually; loopback policy differs from Linux namespaces, so the host-loopback refusal checked by S-USE-014 is not a macOS guarantee.

## Preparation and assertion functions

The product fixture creates separate `CASE_ROOT`, `WS`, HOME, XDG and Session data directories for every scenario. Checks run in a fresh `bash -euo pipefail` and do not inherit a preceding Scrut check's shell state. `--subject-bin` selects an existing product binary. When running Scrut directly, set `SUBJECT_BIN` to its absolute path; USE scenarios otherwise resolve `pvisor` through PATH.

Preparation and assertion source: [fixtures/use.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/use.bash).

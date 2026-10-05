# Learn pVisor through tasks

Start with a trusted script, then stage files, inspect execution evidence, save proposals, and explore branches before setting boundaries for untrusted work. Each chapter explains when to use a capability, which command to run, and what result to expect.

| Order | Your question | Commands | Executable cases |
|---|---|---|---|
| [1. First Job](01-first-job.md) | How do I record scripts, failures, and deadlines? | `run`, `status`, `--config` | S-USE-001–004 |
| [2. Review and decide](02-review-and-decide.md) | What did the Agent change, and what should I accept? | `review`, `inspect`, `apply`, `drop`, `kill` | S-USE-005–008 |
| [3. Save and branch](03-checkpoint-and-fork.md) | How do I retain a proposal and compare independent attempts? | `checkpoint create/list/show`, `fork` | S-USE-009–011 |
| [4. Set boundaries](04-boundaries.md) | How do I restrict file access and network connections? | `--safe`, `--access`, `--overlaynet-deny-all` | S-USE-012–014 |
| [5. History and restoration](05-tools-and-restoration.md) | Should I restore files, Agent history, or the whole VM? | `replay`, capability inspection | S-USE-015–016 |

## Complete one file review

After installation, run these commands in a test directory. Use an absolute stage path outside the workspace so execution records are not treated as project files. The original directory has no report.txt after the command; inspect reads the proposal, and apply creates the file in the original directory.

```bash
sandbox=$(mktemp -d)
mkdir "$sandbox/workspace"
cd "$sandbox/workspace"
pvisor run --stage "$sandbox/draft" -- /bin/sh -c 'printf proposal > report.txt'
pvisor review "$sandbox/draft" --diff
pvisor inspect "$sandbox/draft" -- /bin/cat report.txt
pvisor apply "$sandbox/draft" --path report.txt
cat report.txt
```

This stages workspace files while host file access remains ambient. Before running an untrusted Agent, continue to chapter 4, inspect actual isolation evidence, and read about [execution environments](../guides/executors/index.md).

## Choosing companion tools

| Need | Tool and entry point | Next step |
|---|---|---|
| Review and approve access interactively | `pvisor run --tui` / `--ask`, dispatched to adjacent `pvisor-tui` | [CLI reference](../reference/cli.md) |
| Route real model requests and capture trajectories | `pvisor run --gateway-mode capture --gateway-route ...` | [Capture guide](../guides/capture.md) and the local mock-request test in `just examples 04-gateway-llm-control` |
| Prepare restoration from native Agent history | `pvisor replay`, dispatched to adjacent `pvisor-replay` | [Chapter 5](05-tools-and-restoration.md), [replay guide](../guides/replay.md) |
| Select an OCI container or VM executor | `pvisor run --executor container` / `--executor vm` | [Containers](../guides/executors/container.md), [VMs](../guides/executors/vm.md); requires a runtime/rootfs |
| Save CPU, RAM, devices, and the complete file tree | Cluster execution profile / storage SDK | [Full environment snapshots](../design/environment-snapshot.md); the standalone snapshot CLI is removed and ordinary Job execution support remains limited |
| Manage OCI file caching or a shared cold-page pool | `pvisor service cache` / `pvisor service memory-pool` | [Shared image cache](../reference/shared-image-cache.md), [memory sharing](../design/memory-sharing/index.md) |

Gateway capture requires a build with the gateway feature; wheels and `just build release` include it. Core Job commands work without companion binaries; `pvisor --help` lists installed optional commands in their object group. Full VM snapshots require KVM or Apple Silicon Hypervisor and a working FUSE backend. Ordinary VM Jobs support execution checkpoints with a compatible owned-rootfs, no-network profile; `status --json` reports capability and blockers.

## Execute the documentation

Each S-USE case has one Bash block containing product commands, semantic assertions, and failure conditions. Functions such as journey_setup and json_expect are repository test fixtures, so a full block cannot be pasted into an ordinary shell. Follow the pvisor commands to learn the workflow; use these entries to run the complete checks.

```bash
just semspec --config semspec-use.toml list --domain USE
just semspec --config semspec-use.toml lint
just cases-v2
just cases-v2 --case S-USE-005,S-USE-007 --keep
python3 scripts/cases/run.py --subject-bin target/release/pvisor --output target/pvisor-learning-report.json
```

`just cases-v2` builds the release product and companions, then executes all 16 cases in isolated temporary workspaces, HOME, XDG, and Job data directories. Linux CI checks FUSE and user/mount/network namespaces first. Missing prerequisites do not become SKIP; environment and execution failures fail the gate. The report is target/pvisor-learning-report.json. Failures retain their workspaces; --keep also retains successful ones. Selected runs require exactly the requested IDs. Full runs discover all IDs from this documentation directory, automatically including new cases. Empty reports, missing cases, duplicates, SKIP, XFAIL, and any other non-PASS verdict fail the gate.

Specifications, Python assertions embedded in the fixtures, and Bash vocabulary participate in semspec digests. New cases remain UNREVIEWED: execution success and human semantic approval are separate. After human review of the engine, vocabulary, and cases, add --require-reviewed. This change does not edit the review ledger.

The existing [DOC cases](../reference/cases.md), [VM control cases](../reference/cases-vm.md), just cases, just vm-cases, and examples/pvisor keep their entries. CI runs existing isolation regressions and network/Gateway mock scenarios alongside this learning path. Retired standalone snapshot hardware records remain historical evidence. Use `just test-service-vm` for current capped VM environment-sharing acceptance, and validate complete execution restoration separately through the Cluster VM guide. This learning path does not count unexecuted VM/Gateway capabilities as success.

The full execution gate currently targets Linux. On macOS, select applicable cases individually; loopback policy differs from Linux namespaces, so the host-loopback refusal checked by S-USE-014 is not a macOS guarantee.

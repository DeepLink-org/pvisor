# What is PolicyVisor?

**PolicyVisor (pVisor)** provides **policy-governed, reviewable execution** for existing Agent CLIs, scripts, and automation commands. You keep your tools; pVisor manages capability admission, runtime controls, workspace staging, and a local execution record.

The **p** stands for **Policy**. A Run connects the authority you request with the controls actually installed and the effects available for review. See [the PolicyVisor model](../../zh/concepts/policyvisor.md) for how these fit together.

```bash
pvisor run --safe --stage ../task-stage -- codex
pvisor status --review last
pvisor apply last --all
```

!!! tip "Choose a reviewable workspace"

    Ordinary host Jobs write through. `--safe` and `--ask` retain a staged workspace by default; `--stage PATH` selects its location. See [staging and storage](../reference/cli.md#staging-and-storage) for the separate HOME and VM rootfs lifetimes.

## What you get

- **Capability admission and runtime controls:** evaluate requested authority against the selected executor and record effective controls and any degradation.

- **A Run record:** command, executor, outcome, warnings, and the controls actually installed.
- **A staged workspace:** inspect changed files and apply selected batches or discard the remainder.
- **Optional network policy and Gateway capture:** control mediated traffic and record model requests and responses.
- **Logical checkpoints and forks:** preserve a staged filesystem state and start a related Run.

## Where the guarantees stop

The host, container, and VM executors have different boundaries. A staged directory does not prove that every host path or network connection is isolated. Read the warnings and capability evidence in `pvisor status --review`.

`apply` and `drop` govern staged files. They cannot undo an external API call, a database write, or a message already sent. Logical checkpoints do not save process memory. Distributed scheduling and hostile multi-tenant operation are outside the current local workflow.

Start with [your first Run](first-run.md), then choose a [task guide](../../zh/guides/index.md).

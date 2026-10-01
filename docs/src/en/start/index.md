# Start here

**PolicyVisor (pVisor)** provides policy-governed, reviewable execution for Agent CLIs, scripts, and automation commands. It records effective controls and stages workspace changes; retain the stage to review and apply them.

The Python package and CLI are both named `pvisor`.

## Your first workflow

1. [Install pVisor](installation.md) and check the platform requirements.
2. [Run a small example](first-run.md) without an Agent account or API key.
3. Replace the example command with your script, automation command, or Agent CLI.
4. [Review and apply](../../zh/guides/review-apply.md) the changes you want to keep.

Use `--safe` or `--stage PATH` to stage workspace changes for review after exit.
See [staging and storage](../reference/cli.md#staging-and-storage) for defaults and
the separate HOME and VM rootfs lifetimes. Filesystem staging does not undo
network requests or changes to external services.

## Find an answer

| Question | Read |
| --- | --- |
| What does pVisor do? | [Product overview](what-is-pvisor.md) |
| Where should the command run? | [Host, container, or VM](../../zh/guides/execution.md) |
| What is actually isolated? | [Capabilities and evidence](../../zh/concepts/capabilities-and-evidence.md) |
| How do I record model traffic? | [Capture](../../zh/guides/capture.md) |
| Which option do I need? | [CLI reference](../reference/cli.md) |
| How do I build or contribute? | [Development](../../zh/development/index.md) |

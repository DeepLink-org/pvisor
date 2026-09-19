# Start here

**PolicyVisor (pVisor)** provides policy-governed, reviewable execution for Agent CLIs, scripts, and automation commands. It records effective controls and, with staging enabled, lets you review file changes before applying them.

The CLI remains `pvisor`. The published Python package is still `persisting`, and repository links retain the existing `Persisting` path.

## Your first workflow

1. [Install pVisor](installation.md) and check the platform requirements.
2. [Run a small example](first-run.md) without an Agent account or API key.
3. Replace the example command with your script, automation command, or Agent CLI.
4. [Review and apply](../guides/review-apply.md) the changes you want to keep.

Use `--stage` for reviewable workspace changes. Without it, the command may write directly to the project. Filesystem staging does not undo network requests or changes to external services.

## Find an answer

| Question | Read |
| --- | --- |
| What does pVisor do? | [Product overview](what-is-pvisor.md) |
| Where should the command run? | [Host, container, or VM](../guides/execution.md) |
| What is actually isolated? | [Capabilities and evidence](../concepts/capabilities-and-evidence.md) |
| How do I record model traffic? | [Capture](../guides/capture.md) |
| Which option do I need? | [CLI reference](../reference/cli.md) |
| How do I build or contribute? | [Development](../development/index.md) |

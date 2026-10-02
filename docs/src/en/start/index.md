# Start here

**PolicyVisor (pVisor)** runs agents unattended and lets you keep only the file changes you approve. It runs your existing Agent CLI, script, or automation command and records the limits that actually applied to each execution.

The Python package and CLI are both named `pvisor`.

## Complete your first loop

1. [Install pVisor](installation.md) and check platform prerequisites.
2. [Run the small demo](first-run.md); no agent account or API key is needed.
3. Replace the demo command with your script, automation, or Agent CLI.
4. [Review and apply](../guides/review-apply.md) the changes you want to retain.

Use `--safe` or `--stage PATH` to stage workspace changes for review after exit. Defaults and the different handling of HOME and VM rootfs are defined in [staging and storage](../reference/cli.md#暂存与存储).

## Find an answer

| Question | Documentation |
| --- | --- |
| What does pVisor do? | [Product overview](what-is-pvisor.md) |
| Where should my command run? | [Host, container, and VM](../guides/executors/index.md) |
| What was actually isolated? | [Capabilities and evidence](../concepts/capabilities-and-evidence.md) |
| How do I record model requests? | [Capture](../guides/capture.md) |
| Which option should I use? | [CLI reference](../reference/cli.md) |
| How do I build and contribute? | [Community](../community/index.md) |

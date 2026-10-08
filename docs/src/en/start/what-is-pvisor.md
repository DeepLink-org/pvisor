# What is PolicyVisor?

**PolicyVisor (pVisor) runs agents unattended; you decide which file changes to keep.**

It runs your existing Agent CLI, script, or automation command: the command runs unattended inside a policy boundary, and file changes go to a stage first. Afterwards you review changes and evidence like a pull request and apply only what you want to keep.

pVisor provides a shared file-review, selective-merge, and run-record workflow across agents and executors. See [why pVisor](../why/index.md) for the path to scaling autonomous execution, and [comparisons](../why/comparisons.md) for point-by-point comparisons with Docker, built-in agent sandboxes, and similar approaches.

## What you get

- **Hands off:** no babysitting approval prompts. Agent file changes go to a stage first; network and sensitive paths follow policy.
- **Gate the result:** review changes like a pull request, choose which paths to apply, and discard the rest in one step. If you edited the same file meanwhile, pVisor refuses to overwrite your changes.
- **Keep a record:** every run leaves a checkable record of the limits that actually applied, the accesses it blocked, and optionally the model requests.

## Three commands

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

!!! tip "Try a demo that needs no API key"

    [Your first run](first-run.md) uses a "fake agent" script to walk the full loop: it edits source, deletes a file, attempts to read a sensitive path and reach the internet, and you see the changes and blocked accesses during review before applying only `src`.

## Where we are today

The current workflow uses local Jobs: run an agent, review the result, and selectively merge file changes. See the [trust ladder](../why/trust-ladder.md) for the roadmap and [executor boundaries](../security/executor-boundaries.md) for each executor's protection scope.

## Guarantee scope

A run's boundary comes from its capability evidence; `apply` and `drop` manage staged files only and do not undo external side effects. See [capabilities, evidence, and guarantee boundaries](../concepts/capabilities-and-evidence.md) for the full scope and [security](../security/index.md) for the threat model.

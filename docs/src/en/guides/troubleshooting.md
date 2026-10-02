# Troubleshoot a Run

When a Run behaves unexpectedly, inspect the Run Bundle before repeating the command with stronger options. It records requested boundaries, installed mechanisms and warnings limiting what the Run can claim.

## Three read-only checks

Run these in the project directory where the command started:

```bash
pvisor status last
pvisor inspect last -- git status --short
pvisor status --review last
```

`status` distinguishes a running Run from a stopped one. `inspect` runs a read-only command in the Run view to distinguish staged changes from real project changes. `status --review` shows the persisted Bundle and evidence before you decide to apply or drop.

## The agent changed the project directly

Check whether the launch command enabled staging:

```bash
pvisor run --stage ../stage-task-001 -- AGENT_COMMAND
```

Without staging, the host executor does not produce staged filesystem effects for review/selective apply; it also preserves the host filesystem view by default. Use `--filesystem sandbox` for access restrictions and `--stage` for reviewing changes. If staging was intended, inspect the recorded path and executor warnings before rerunning.

## A requested capability was not enforced

Treat options as intent rather than evidence. Inspect actual capability records and mechanisms in the Bundle; see [Capabilities and evidence](../concepts/capabilities-and-evidence.md). Executors differ: a cooperative network proxy cannot guarantee all ambient connections are blocked.

## The stage is empty or contains unexpected files

Check the working directory and stage path:

```bash
pvisor inspect last -- pwd
pvisor inspect last -- git status --short
```

The agent modifies the Run-owned view. Writes beyond that view may be recorded as external effects or may be unsupported by the executor. Keep the stage outside the project and use an explicit path to identify the Job; do not compare generated Run directories with the project root by filename alone.

## Capture output is missing

Execution review and model traffic capture are separate choices. Check that the Run completed and the Bundle is readable, then inspect capture configuration and the `--record-destination` path.

If the directory is empty, read [Capture an agent trajectory](capture.md) and confirm configuration before creating another Run. A local Run Bundle is the execution record; model requests/responses are included only when capture is enabled.

## Before filing an issue

Provide pVisor version, OS, executor, complete command and relevant `status`/`status --review` output, with credentials and private workspace contents removed. Explain which capability was requested, which mechanism the Bundle recorded and where actual behavior differed.

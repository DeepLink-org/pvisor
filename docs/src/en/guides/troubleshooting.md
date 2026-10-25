# Troubleshoot a Run

If a Job behaves unexpectedly, inspect its Run Bundle to check requested boundaries, installed mechanisms and warnings before deciding whether to change options and retry.

## Do three read-only checks first

Run these in the project directory where the command started. If you used an explicit stage, replace `last` with that path. [Jobs and storage](../concepts/jobs.md) explains how to find the task again:

```bash
pvisor status last
pvisor inspect last -- git status --short
pvisor status --review last
```

`status` tells you whether the Run is still running or has stopped. `inspect` runs a read-only command in the Run view, which separates staged changes from changes in the real project. `status --review` shows the persisted Run Bundle and Evidence to help you decide before you apply or drop.

## The agent changed the project directly

First check whether the launch command used staging:

```bash
pvisor spawn --stage ../stage-task-001 -- AGENT_COMMAND
```

Without staging, the host executor produces no staged filesystem Effect that you can review and apply selectively, and it also preserves the host filesystem view by default. Use `--filesystem sandbox` when you need filesystem access restrictions, and add `--stage` when you need to review changes. If you did intend to stage, inspect the recorded stage path and executor warning, then run again.

## A requested capability was not enforced

Treat request options as intent, not evidence. Open the Run Bundle and look at the actual capability records and their mechanisms; for how to read evidence, see [Capabilities and evidence](../concepts/capabilities-and-evidence.md). Support varies by executor: a cooperative network proxy cannot guarantee that every ambient connection is blocked.

## The stage is empty or has the wrong files

First check the command's working directory and the stage path:

```bash
pvisor inspect last -- pwd
pvisor inspect last -- git status --short
```

The agent modifies the Run-owned view. Writes outside that view may be recorded as an external Effect or may be something the executor cannot provide. Keep the stage outside the project directory, identify the Job by an explicit stage path, and do not compare the generated Run directory with the project root by filename alone.

## Capture output is missing

Execution review and model traffic capture are two independent decisions. First confirm the Run has finished and the Bundle is readable, then check the capture configuration and the target path passed to `--record-destination`.

If the target directory is empty, check proxy or base-URL configuration using the [model traffic capture steps](capture.md), then start a new Job. Use the Run Bundle to inspect execution results; with capture enabled, model requests and responses are recorded in the Gateway Journal in the capture directory.

## Before filing an issue

Provide the pVisor version, operating system, executor, the full command, and the relevant `status` and `status --review` output, with credentials and private workspace contents removed. The most helpful report says which capability was requested, which mechanism the Bundle recorded, and where the actual result differed.

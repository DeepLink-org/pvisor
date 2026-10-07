# Start here

**PolicyVisor (pVisor)** runs agents unattended and lets you keep only the file changes you approve. It runs your existing Agent CLI, script, or automation command and records the limits that actually applied to each execution.

The Python package and CLI are both named `pvisor`. Follow [installation](installation.md) and check platform prerequisites first: staged host Jobs need FUSE/macFUSE, while ordinary host Jobs write directly to the workspace by default.

## Complete your first loop

From a disposable test project directory with no existing `result.txt`, verify staging and review with an offline command. No agent account or API key is needed:

```bash
pvisor run --safe --overlaynet-deny-all -- /bin/sh -c 'printf "candidate\n" > result.txt'
pvisor status --review last
pvisor inspect last -- cat result.txt
```

Expect exit code zero and no new `result.txt` in the original project yet. After the Job stops, `inspect` runs the host's `cat` in a kernel-read-only OverlayFS workspace view and should print `candidate`. Review terminal status, installed controls, and file changes, then choose whether to keep or discard the result.

To keep the file, run:

```bash
pvisor apply last --path result.txt
```

Or discard the candidate instead:

```bash
pvisor drop last
```

This example has only one change, so apply completes it and cleans up disposable staging data; no subsequent drop is needed. When there are multiple changes and you apply in batches, unselected candidates remain for a later apply or drop. The full [first run](first-run.md) also demonstrates file deletion and access interception.

## Use a real task

Replace the command after `--` with an installed script or Agent CLI, explicitly configuring the credentials and network destinations it needs.

To choose storage, use `--stage PATH` with a fresh directory outside the project, then select that path or the printed Job ID in later commands. Do not rely on `last`, which searches default storage only. See [staging and storage](../reference/cli.md#暂存与存储) for write destinations in HOME, the VM root, and the workspace. Remote APIs, database writes, and messages are outside file staging.

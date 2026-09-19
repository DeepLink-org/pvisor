# Your first Run

This example creates one file in a staged project, reviews it, and applies it. It needs no Agent account or model API. Complete [installation](installation.md) first, including FUSE/macFUSE for staged host execution.

## 1. Create a disposable project

```bash
mkdir -p pvisor-demo/project
cd pvisor-demo/project
printf 'original\n' > original.txt
```

## 2. Run a command in a stage

```bash
pvisor run --stage ../stage-001 -- /bin/sh -c 'printf "hello from the stage\n" > hello.txt'
```

The working-directory view is staged. The stage is outside the project so its metadata does not clutter the project tree. Use a new stage directory for another Run.

```bash
test ! -e hello.txt
pvisor review last
```

`hello.txt` is absent from the base project and appears in the review. Read the reported isolation warnings as well as the file list. If the stage cannot be mounted, fix the platform setup before continuing.

## 3. Accept the change

```bash
pvisor apply last --path hello.txt
cat hello.txt
```

The base project now contains `hello from the stage`. To reject an unapplied stage instead, use `pvisor drop last`. Drop does not undo files already applied.

## 4. Use your own command

From a real project, replace the shell example with your script or automation command. An installed Agent CLI works through the same entry point:

```bash
pvisor run --stage ../agent-stage-001 -- codex
pvisor review last
```

Review first, then choose `pvisor apply last --path PATH`, `--all`, or `pvisor drop last`. When working across several projects or Runs, use the explicit Run ID or stage path printed by the command instead of `last`.

Continue with [review and apply](../guides/review-apply.md) for batching, conflicts, and checkpoints.

# Why use pVisor?

An Agent, script, or automation command can produce useful changes while also touching files or services beyond the intended task. Its output alone does not show the complete workspace result or the controls applied during execution.

PolicyVisor (pVisor) connects declared authority, effective controls, and recorded outcomes. With `--stage`, it also puts a review step between file changes and the base project, so you can decide what to keep using the execution record and workspace diff.

Use it when you want to run an existing command, inspect its workspace changes, apply them in selected batches, and retain a local execution record. Add capture when you also need model traffic.

This workflow does not replace version control, backups, application-level authorization, or the Agent framework. Filesystem review cannot undo remote side effects. Choose an executor and network policy for the boundaries your task actually requires.

Try [the first-Run example](../start/first-run.md) or inspect [capabilities and evidence](capabilities-and-evidence.md).

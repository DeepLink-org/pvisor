# Using PolicyVisor

Choose the smallest workflow that answers the question in front of you.

## I need to review changes before they reach my project

Start with [your first Run](../start/first-run.md): run an Agent CLI, script, or
automation command with `--stage`, review the Run Bundle, and apply the paths
you choose.

## I need to constrain access during execution

Choose an [executor](execution.md) and configure [network policy](network.md)
for the task. Inspect [capabilities and evidence](../concepts/capabilities-and-evidence.md)
to distinguish requested authority from actual enforcement.

## I need a record of model traffic

Use [pVisor capture](capture.md) when a Run should keep the
model requests and responses it actually sent. The private Run Bundle remains
the local execution record.

## A reliable operating habit

1. Start with one Run.
2. Record the exact command, path, and provider.
3. Review the result before applying or sharing it.
4. Keep the Run Bundle with any conclusion.
5. Automate only after the manual path is repeatable.

See [Design principles](../design/principles.md) for the
implementation background.

# Using Persisting

Choose the smallest workflow that answers the question in front of you.

## I need an Agent to change a project safely

Start with [pVisor](../pvisor/get-started.md): run one Agent in a staged
workspace, review the Run Bundle, and apply only a trusted path. Add network or
provider controls when the next Run needs them.

## I need a record of model traffic

Use [pVisor capture](../pvisor/guides/capture.md) when a Run should keep the
model requests and responses it actually sent. The private Run Bundle remains
the local execution record.

## A reliable operating habit

1. Start with one Run.
2. Record the exact command, path, and provider.
3. Review the result before applying or sharing it.
4. Keep the Run Bundle with any conclusion.
5. Automate only after the manual path is repeatable.

See [Design principles](../system-design/design-principles.md) for the
implementation background.

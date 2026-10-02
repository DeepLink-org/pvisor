# What is PolicyVisor?

**PolicyVisor (pVisor) runs agents unattended; you decide which file changes to keep.**

It runs your existing Agent CLI, script, or automation command within a policy boundary. With staging enabled, file changes wait in a stage. After execution, review changes and evidence like a pull request and apply only what you want.

Agent CLIs often include sandboxes or approval modes. Blocking actions alone does not answer what changed, what to keep, or what record can be checked across agents and executors. See [why pVisor](../why/index.md) for the supervision argument and [comparisons](../why/comparisons.md) for other approaches.

## What you get

- **Hands off:** file changes are staged; network and sensitive paths follow policy.
- **Gate the result:** review and apply selected paths, discard the rest. pVisor refuses to overwrite conflicting edits you made meanwhile.
- **Keep a record:** inspect installed controls, blocked accesses, and optionally model requests.

## Three commands

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

!!! tip "Try a demo without an API key"
    [Your first run](first-run.md) uses a script to edit source, delete a file, attempt sensitive reads and network access, then apply only `src`.

## Where we are today

Today pVisor runs individual Jobs locally: one agent finishes unattended, then you review every change and merge selectively. These are the foundations for the higher levels in the [trust ladder](../why/trust-ladder.md). See [executor boundaries](../security/executor-boundaries.md) for their scope.

## Guarantees

A run's boundary comes from its capability evidence. `apply` and `drop` manage staged files and do not undo external effects. See [capabilities and evidence](../concepts/capabilities-and-evidence.md) and [security](../security/index.md).

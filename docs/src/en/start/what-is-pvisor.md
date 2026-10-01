# What is PolicyVisor?

**PolicyVisor (pVisor) runs agents unattended and keeps only the file changes you approve.**

Many Agent CLIs now ship their own sandbox or approval mode. They answer "can it be blocked?", but not "what exactly changed, which parts should I keep, and what is the checkable record?"—and they do not span agents or executors. PolicyVisor adds those three things: it runs your existing Agent CLIs, scripts, or automation commands unattended inside a policy boundary, and you review the result like a pull request, keeping only the changes you want.

## Why "scale"

The limit on agent autonomy is not compute; it is human supervision. Today every unit of agent work costs roughly the same unit of human attention—approving up front, reading through afterwards, or cleaning up when something breaks. Supervision cost grows with execution volume, so autonomy is capped by attention.

Decoupling supervision from execution volume takes three properties at once:

| Property | Meaning | What it buys you |
| --- | --- | --- |
| Bounded | the blast radius is known up front | no need to approve every action |
| Recoverable | staged changes can be selectively merged, discarded, or forked before apply | failure is cheap, so you can let go |
| Checkable | execution leaves checkable evidence | review can be sampled, then automated |

PolicyVisor is the execution layer that gives agent execution these three properties. Delegation scale comes from policy and evidence, not from the time you spend watching.

## What you get

- **Hands off.** No approval prompts to babysit. The agent's file changes land in a stage; network and sensitive paths follow your policy.
- **Gate the result.** Review the changeset like a PR, apply the paths you want, discard the rest. If you changed the same file meanwhile, pVisor refuses to overwrite your edit.
- **Keep a record.** Every run leaves a checkable record of the limits actually in effect, the access that was blocked, and optional model requests.

## Why not an existing tool

| Option | What it does | What it lacks |
| --- | --- | --- |
| Docker / devcontainer | isolated, reproducible environment | change review, conflict-refusing overwrite, per-path selective merge, and evidence of the limits in effect are still yours to build |
| An agent's own sandbox | blocks some commands | all-or-nothing approval, agent- and executor-specific, no checkable record |
| git worktree | filesystem-level isolation | no network or credential control, no evidence |
| Cloud sandbox (e.g. E2B) | remote isolated execution | separate from your local toolchain and workspace |
| Kubernetes / Ray | scheduling and orchestration | schedules processes, not "bounded, recoverable, checkable" executions |

When you do not need pVisor: if you just want a throwaway sandbox you can discard with `git checkout`, and you do not need checkable evidence or selective merge, an agent's own sandbox or Docker is enough.

## Where this stands today

Today pVisor runs one Job at a time on your machine: let one agent finish unattended, then review every change and selectively apply. That level already carries the three properties the higher levels need—policy and evidence deciding what can skip review, up to post-hoc audit and many agents in parallel, and finally clustered execution.

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # or: pvisor drop last
```

!!! tip "Try the demo that needs no API key"

    [Your first run](first-run.md) uses a fake agent script that edits source, deletes a file, tries to read a sensitive path, and tries to reach the network. The review shows the changes and the blocked access, and you apply only `src`.

`--safe` keeps workspace changes in Job storage, and `last` resolves the latest Job for the current workspace. With `--stage PATH`, pass that path instead of `last`.

## Scope

A run's boundary is its capability evidence. `apply` and `drop` govern staged files only, with no undo for external effects. See [capabilities and evidence](../../zh/concepts/capabilities-and-evidence.md) for the full scope.

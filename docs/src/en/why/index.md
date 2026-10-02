# Why pVisor

**The limit on agent autonomy is human supervision, not compute.**

## Where supervision costs come from

Delegating work to an agent costs attention in one of three ways:

| Approach | Human work | Cost grows with |
| --- | --- | --- |
| Approval beforehand | Confirm commands and file changes individually | Every execution step |
| Investigation afterward | Read diffs and logs to find boundary violations | Change volume and uncertainty |
| Cleanup after failure | Restore deletions, rotate leaked credentials, clean up effects | Blast radius |

All three tend to require roughly the same unit of human attention for each unit of agent work. Supervision therefore grows linearly with execution volume, capping delegation regardless of model capability. Multiple unattended agents or team pipelines hit the same limit.

## Decouple supervision from execution volume

Each execution needs all three properties:

| Property | Meaning | Supervision cost reduced |
| --- | --- | --- |
| Bounded | Know the blast radius in advance | Per-command approval: worst cases already have a boundary |
| Recoverable | Selectively apply, discard, or fork staged files | Cleanup: failed file results can be discarded |
| Checkable | Leave evidence that can be checked | Investigation: sample results, eventually automate assessment |

A boundary without evidence still requires reading everything. Evidence without recovery may arrive too late. Recovery without a boundary leaves unstaged side effects uncontrolled.

## pVisor's role

pVisor is the **execution layer** providing these properties:

- Beyond a sandbox boundary, it supplies staging, selective merging, and evidence of controls actually installed.
- It defines execution semantics across host, container, and VM. Future integration with Kubernetes or Ray complements their scheduling.
- Claude Code, Codex, and arbitrary scripts share one entry point and semantics.

Delegation can then depend on policy and evidence instead of time spent watching.

## Today and the direction

Today pVisor delivers L1: one local agent finishes unattended; you review every change and merge selectively. Policy-based exemptions, multiple agents, and cluster execution build on the same properties; see the [trust ladder](trust-ladder.md).

The progress metric is **human supervision cost per unit of agent work**: interventions and time spent. See the planned [supervision cost study](../benchmarks/supervision-cost.md).

## Continue reading

- [Trust ladder and scale](trust-ladder.md)
- [Use cases](use-cases.md)
- [Comparisons](comparisons.md)
- [When not to use pVisor](when-not-to-use.md)
- [FAQ](faq.md)

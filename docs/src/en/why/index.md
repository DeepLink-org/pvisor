# Why pVisor

Give an agent a task, then walk away for two hours—few people dare to do that today. It is not that models fall short; it is that you cannot confirm what the agent did during those two hours.

## The bottleneck is supervision, not compute

How long an agent can run depends on how long you are willing to watch. Approve every command and human attention becomes the ceiling; go fully automatic and you must accept that it may corrupt files, read what it should not, and send requests where they should not go.

Scale stalls here: **every unit of agent work requires roughly one unit of human attention**. As execution volume rises, supervision cost rises linearly, and autonomy is pressed down to human bandwidth.

## Decouple supervision from execution volume

To let go, every execution needs three things at once:

| Property | Meaning | What it means for you |
| --- | --- | --- |
| Bounded | The blast radius of a mistake is known in advance | No need to approve every command |
| Reversible | Changes can be selectively merged, discarded, or forked before they are applied | Failure is cheap, so you dare to let go |
| Checkable | Execution leaves a record you can verify | Review can sample, or even be handed to a machine |

pVisor is the execution layer that gives every execution these three properties.

## What it is, and what it is not

pVisor is the **semantic layer for each execution**: it defines boundaries, installs controls, and leaves evidence.

It is not a sandbox—the isolation substrate is provided by the executor (host namespaces/Seatbelt, containers, VMs). It is not a scheduler either—cross-node orchestration goes to Kubernetes and Ray, while pVisor provides consistent semantics and evidence for scheduled execution.

## Today, and after

Today pVisor runs Job by Job on one machine: finish a task unattended, then afterward merge only the changes you want, as you would review a PR. This is L1, and it already has the three properties needed for higher levels of autonomy.

For how the four levels split, where pVisor places its bet, and the status of L2/L3, see [trust ladder and scale](trust-ladder.md); for usage at different scales see [use cases](use-cases.md).

!!! note "Under construction"
    "Human supervision cost per unit of agent work" has no measured data yet; requirements and acceptance criteria are in [supervision cost (planned)](../benchmarks/supervision-cost.md).

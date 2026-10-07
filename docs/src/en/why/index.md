# Why pVisor

Letting an agent complete a task unattended calls for limits on file and network access, followed by a review of its changes and execution record.

## Reduce intervention during execution

How long an agent can run depends on how long you are willing to watch. Approve every command and human attention becomes the ceiling; go fully automatic and you must accept that it may corrupt files, read what it should not, and send requests where they should not go.

More tasks take more attention when every command needs approval. Execution boundaries and post-run review can reduce intervention while tasks run.

## Decouple supervision from execution volume

To let go, every execution needs three things at once:

| Property | Meaning | What it means for you |
| --- | --- | --- |
| Bounded | The blast radius of a mistake is known in advance | No need to approve every command |
| Reversible | Changes can be selectively merged, discarded, or forked before they are applied | Failure is cheap, so you dare to let go |
| Checkable | Execution leaves a record you can verify | Review can sample, or even be handed to a machine |

pVisor is the execution layer that gives every execution these three properties.

## Responsibilities of the execution layer

pVisor is the **semantic layer for each execution**: it defines boundaries, installs controls, and leaves evidence.

Executors provide isolation through host namespaces/Seatbelt, containers, or VMs. pVisor manages execution policy, lifecycle, and records; external systems such as Kubernetes and Ray handle cross-node orchestration.

## Today, and after

Today pVisor runs Job by Job on one machine: finish a task unattended, then afterward merge only the changes you want, as you would review a PR. This is L1, and it already has the three properties needed for higher levels of autonomy.

For how the four levels split, where pVisor places its bet, and the status of L2/L3, see [trust ladder and scale](trust-ladder.md); for usage at different scales see [use cases](use-cases.md).

!!! note "Under construction"
    "Human supervision cost per unit of agent work" has no measured data yet; requirements and acceptance criteria are in [supervision cost](../benchmarks/supervision-cost.md).

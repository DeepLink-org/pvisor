# Should you choose local pVisor or E2B, Daytona and Modal?

## Main conclusions {#conclusions}

**pVisor suits existing local repositories and toolchains; E2B, Daytona and Modal suit remote environment provisioning and managed capacity.** Matching cloud latency and billing data are unavailable, so pVisor cannot be claimed faster or cheaper.

Prepared local pVisor VM startup takes about **0.1 s**, staged short repair about **0.7 s**. Remote execution also needs upload, preparation, execution and result return; environment creation alone does not describe that cost.

| Need | Selection implication |
|---|---|
| Local repository and tools are ready | Evaluate pVisor local feedback time |
| Remote environments and managed capacity | Evaluate E2B, Daytona and Modal |
| Compare costs or speed | Requires same-region end-to-end measurements |

## Motivation {#motivation}

Local/cloud choice depends on repository location, existing tools, elastic capacity and returning changes to the workspace, as well as startup.

## Experiment design {#interpretation}

This compares deployment approaches. Performance evidence is local only; no cloud sandbox was created. SDK, region, account, caches, latency and bills are unmeasured. Official sources describe provisioning rather than supplying advertised numbers for rankings.

| Option | Execution/preparation | User workflow |
|---|---|---|
| Local pVisor | Host directories or prepared rootfs, stage/apply | Local resources, dependencies and operations |
| E2B | SDK-operated remote sandboxes, templates and files | Templates, uploads, result return and local application |
| Daytona | SDK-managed remote development sandboxes | Preparation, synchronization and result application |
| Modal | SDK-created sandboxes, images and storage | Dependency images, data access and pipeline integration |

Official sources: [E2B](https://docs.e2b.dev/), [Daytona](https://www.daytona.io/docs/en/), [Modal Sandboxes](https://modal.com/docs/guide/sandboxes).

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

[Startup](startup.md), [tasks](agent-tasks.md) and [density](density.md) provide local short-task budgets, without establishing managed-service capacity or availability.

A matched task comparison should record builds, uploads, creation, dependency caches, execution, downloads and application. Persistent environments amortize preparation; repeated large-repository uploads can increase remote waiting. Cloud snapshots or pause/resume do not automatically provide conflict handling for local directories.

Costs include resource time, storage/network, subscriptions and models; local costs also include hardware and operations. Without billing experiments, the choice is conditional: evaluate managed services for remote APIs and burst capacity, and pVisor for existing local dependencies and short feedback loops.

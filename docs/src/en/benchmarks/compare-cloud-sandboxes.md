# Comparison: cloud sandboxes

Local pVisor fits existing repositories and tools. E2B, Daytona and Modal supply remote environments and elastic capacity. This first edition compares deployment and cost boundaries; it contains no measured cloud startup or ranking against local CLI timings.

## Scope

Official documentation and billing checked on 2026-10-04. No cloud accounts were called, environments provisioned or paid credits consumed. SDK versions, regions and billing configurations remain unmeasured. Local evidence is in [startup](startup.md), [filesystem](filesystem.md) and [density](density.md).

| Option | Workspace/tools | Capacity, lifecycle and cost | Suitable use |
|---|---|---|---|
| Local pVisor | Host paths or prepared OCI/VM rootfs; configured shares and stage/apply | Host CPU/RAM/disk and operations; measured capacity on this host | Private repositories, local dependencies and changes returned to the same workspace |
| E2B | Remote SDK execution, templates and file transfer | Managed sandboxes, pause/resume; compute billed per second with plan resource/concurrency limits | Independent execution environments within a product |
| Daytona | SDK-managed remote sandboxes, images and file synchronization | Managed lifecycle; per-second billing with compute/storage configuration | Persistent remote agent workspaces |
| Modal | Images, Volumes and uploaded files; gVisor/VM runtimes | Managed capacity; requested resources/time and minimum allocation affect billing | Existing Modal compute, inference and batch pipelines |

Sources: [E2B](https://docs.e2b.dev/), [billing](https://docs.e2b.dev/billing); [Daytona](https://www.daytona.io/docs/en/), [pricing](https://www.daytona.io/pricing); [Modal](https://modal.com/docs/guide/sandboxes), [resources/pricing](https://modal.com/docs/guide/sandbox-resources).

## Data and admission

Remote execution transfers required code, inputs and credentials to the execution location. Cross-border transfer depends on region, account and contract; confirm locality, retention and deletion. Local execution retains workspace files on the host, but model APIs, tools and external record destinations can still transmit data.

For a local Rust/npm workload, measure image build, upload, environment creation, caches, execution, download and local admission separately. Remote snapshots or pause/resume do not automatically provide conflict-safe admission into the local directory; callers need a merge protocol, potentially pVisor staging.

Model cost as resource-seconds plus storage/network/plan charges and inference. Include depreciation, power, idle capacity and operations locally. Without account billing experiments, this page makes no cheapest-provider claim. Evaluate cloud for burst capacity and remote multi-user APIs; evaluate local for host dependencies and short feedback loops.

## Corrections

Send region, SDK version, billing configuration, date and complete timing stages to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues). Label provider-reported numbers separately from controlled measurements.

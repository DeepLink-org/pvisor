# Implementation design

| Area | Documentation |
| --- | --- |
| Core ownership and execution path | [Core architecture](architecture.md) |
| VM RAM offload, file layout and lifecycle | [Complete offload design](offload/index.md) · [Disk format and schema](offload/disk-layout-and-schema.md) |
| Cross-VM content deduplication, shared pool and cold restoration | [Memory deduplication and cold-block compression](memory-sharing/index.md) |
| Operations, rewrites, placement and facts | [Operation and Event](operations-events.md) |
| Host, container, VM and filesystem boundaries | [Isolation](isolation.md) |
| Network policy and interception | [OverlayNet](overlaynet.md) |
| Model routing and capture | [Gateway](gateway.md) |
| CLI and configuration | [Command model](cli.md) |
| Engineering choices | [Design principles](principles.md) |

Implementation claims should map to code and tests. Core design describes current components and execution paths; proposed driver designs must be distinguished from implemented mechanisms. Run Bundle observations determine which controls were actually enforced.

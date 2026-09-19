# Implementation design

This section is for contributors tracing the current execution path. User workflows live in [Guides](../guides/index.md), and exact flags live in [Reference](../reference/index.md).

| Area | Document |
| --- | --- |
| Ownership and data flow | [Architecture](architecture.md) |
| Host, container, VM, and filesystem boundaries | [Isolation](isolation.md) |
| Network policy and interception | [OverlayNet](overlaynet.md) |
| Model routing and capture | [Gateway](gateway.md) |
| CLI and configuration | [Command model](cli.md) |
| Engineering tradeoffs | [Design principles](principles.md) |
| Future distributed operation | [Local to fleet](local-to-fleet.md) |

Read implementation claims against code and tests. Future directions are labeled explicitly; the presence of a contract or diagram does not establish runtime enforcement.

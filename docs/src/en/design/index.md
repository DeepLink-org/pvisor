# Implementation design

| Area | Documentation |
| --- | --- |
| Core ownership and execution path | [Core architecture](architecture.md) |
| Single-node sandbox admission, lifecycle, recovery and storage | [Daemon design](daemon/index.md) |
| Shared image caches, S3/filesystem trees, and file indexes | [v1: independent metadata and paged indexes](shared-image-cache-storage.md) |
| Memory optimization architecture, principles, and tradeoffs | [Overall design](memory-optimization/index.md) · [Deduplication](memory-optimization/deduplication.md) · [Offload](memory-optimization/offload.md) · [Compression](memory-optimization/compression.md) |
| Full VM environment saves, independent file copies and restore across runners | [Full environment snapshot CLI](environment-snapshot.md) |
| Experimental mechanisms and evidence boundaries for memory optimization | [Experimental proof of concept](memory-optimization/proof-of-concept.md) |
| Operations, rewrites, placement and facts | [Operation and Event](operations-events.md) |
| File composition, first-touch and apply recovery | [OverlayCore design](overlayfs.md) |
| Event append, receipts and tail recovery | [Journal design](journal.md) |
| Host, container, VM and filesystem boundaries | [Isolation](isolation.md) |
| Network policy and interception | [OverlayNet](overlaynet.md) |
| Model routing and capture | [Gateway](gateway.md) |
| CLI and configuration | [Command model](cli.md) · [Job checkpoint and fork CLI design](job-checkpoint-cli.md) |
| Engineering choices | [Design principles](principles.md) |

Implementation claims should map to code and tests. Core design describes current components and execution paths; proposed driver designs must be distinguished from implemented mechanisms. Run Bundle observations determine which controls were actually enforced.

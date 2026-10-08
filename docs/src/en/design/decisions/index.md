# Architecture decision records (ADR)

ADRs retain context, alternatives and consequences. Subsystem articles own current implementation contracts; historical decisions retain their original interfaces, versions and evidence scope. An accepted status alone does not establish delivery across every later platform or entry point.

| Number | Decision | Boundary or cost retained | Record status |
| --- | --- | --- | --- |
| 0001 | [Separate plans from installed-control evidence](0001-plan-and-evidence.md) | Plans reach Planned; consumers still inspect observations | Implementation backfill |
| 0002 | [Separate capture and replay](0002-capture-and-replay.md) | Execution works independently; model capture and native trajectories are collected separately | Implementation backfill |
| 0003 | [Keep executor selection explicit under safe](0003-explicit-safe-executor.md) | Reject missing controls; platform prerequisites remain | Implementation backfill |
| 0004 | [Human approval of semantic commitments](0004-human-semspec-approval.md) | Test results and product commitments receive separate review | Existing contribution rule backfill |
| 0005 | [One Rust VM crate and trait API](0005-rust-vm-api.md) | Private backends; uniform declarations do not promise cross-architecture restore | Implementation backfill |
| 0006 | [Snapshot stages and reuse immutable bases](0006-stage-snapshot.md) | Bases require validation and pins; historical standalone CLI profiles differ from current Job interfaces | accepted; retains the original 2026-10-04 status |
| 0007 | [Share file services between host FUSE and virtio-fs](0007-shared-filesystem-service.md) | Shared semantics; adapters retain protocol state and concurrency | Implementation backfill |
| 0008 | [Separate Host and Guest control authority](0008-host-guest-control-authority.md) | Separate credentials/endpoints; internal Host protocols require exact compatibility | Implementation backfill |

Implementation backfill means the choice can be checked against source or existing contribution rules, without new maintainer approval. ADR 0006 retains its independent record's accepted status; its old CLI and measurements remain historical artifacts. [Environment snapshots](../environment-snapshot.md) and [Job checkpoints](../job-checkpoint-cli.md#10-当前实现与验收边界) define current behavior.

## Record the next decision {#new-decision}

Each decision uses a unique four-digit number and a separate `NNNN-short-title.md` file containing context, alternatives, choice, consequences, status and source/evidence scope. The index retains choices and impacts without duplicating bodies. Later changes record supersession relationships and link the original decision.

Distinguish proposed, accepted, superseded, rejected and implementation backfill. Check merged code and human approval separately. Crate-boundary changes update [Migration status](../architecture.md#api-boundaries); format or commit-order changes also update the [Version matrix](../records-and-versions.md) and [Failure semantics](../failure-semantics.md), allowing compatibility and retry conditions to be reviewed together.

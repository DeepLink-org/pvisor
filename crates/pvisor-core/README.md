# pvisor-core

Core definitions for pVisor operations and external interactions. pVisor owns scheduling and execution; observers receive Events describing what happened.

| Module | Responsibility |
| --- | --- |
| `operation` | Operation input, policy decisions, ordered Placement, Outcome and observations |
| `event` | Requested, Rewritten, Placed, Dispatched, Completed and domain observations |
| `execution` | Execution input, identities, capability plans and results |
| `policy`, `network` | Shared authorization definitions and pure evaluation |
| `protocol`, `session` | Existing AgentCtl wire messages and session identities |
| `overlay` | Review/apply records, preimages and local inspection messages |
| `audit` | Approval request/decision contracts and injected approval channel |

The only production operation is `run.execute`. There is no text language or generic rewrite interpreter. Rewritten records actual before/after snapshots; Placement is separate. Events preserve observed causality, not deterministic replay of arbitrary external effects.

Executors, AgentCtl clients/servers, approval socket I/O and lifecycle ownership reside in `pvisor`; mount/network implementations reside in their drivers. Core does not start processes or open control sockets. Policy evaluation does not itself prove enforcement.

AgentCtl remains an optional authenticated cooperative protocol: Hello opens a Session, Sync exchanges state and directives. It is not enforcement evidence. See [the wire contract](src/protocol.rs). The concrete `AgentCtlClient` is exported by `pvisor`, not core.

```sh
just test core
```

[Operation and Event design](../../docs/src/zh/design/operations-events.md) · [System architecture](../../docs/src/zh/design/architecture.md)

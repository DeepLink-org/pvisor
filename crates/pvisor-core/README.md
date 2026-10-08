# pvisor-core

Core definitions for pVisor operations and external interactions. pVisor owns scheduling and execution; observers receive Events describing what happened.

| Module | Responsibility |
| --- | --- |
| `operation` | Operation input, policy decisions, ordered Placement, Outcome and observations |
| `event` | Requested, Rewritten, Placed, Dispatched, Completed and domain observations |
| `execution` | Execution input, identities, capability plans and results |
| `policy`, `network` | Shared authorization definitions and pure evaluation |
| `protocol`, `session` | Cooperative guest AgentCtl wire messages and session identities |
| `host_protocol` | Versioned Host envelopes, target/correlation validation, `HostVmCommand`/`HostVmResult` and typed private supervisor contracts |
| `overlay` | Review/apply records, preimages and local inspection messages |
| `audit` | Approval request/decision contracts and injected approval channel |
| `cpu`, `memory` | Native CPU QoS, usage and memory observation contracts; not scheduler reservations |
| `node` | Immutable local image revision identity (`EnvironmentLayer`) |

The only production operation is `run.execute`. There is no text language or generic rewrite interpreter. Rewritten records actual before/after snapshots; Placement is separate. Events preserve observed causality, not deterministic replay of arbitrary external effects.

Executors, AgentCtl clients/servers, approval socket I/O and lifecycle ownership reside in `pvisor`; mount/network implementations reside in their drivers. Core does not start processes or open control sockets. Policy evaluation does not itself prove enforcement.

Host and Guest AgentCtl use isolated schemas, credentials and endpoints, not shared guest privileges. The optional authenticated cooperative guest endpoint retains `AgentRequest`: Hello opens a Session, Sync exchanges state and directives. It cannot acquire host lifecycle authority and is not enforcement evidence. See [the cooperative wire contract](src/protocol.rs). The concrete `AgentCtlClient` is exported by `pvisor`, not core.

The separate host-authority endpoint uses [`host_protocol`](src/host_protocol.rs): version 1 generic request/response envelopes with request correlation, optional Job/Attempt/generation targets and a common 1 MiB JSON payload limit. Pure validation bounds correlation/target identities to 1–256 bytes and rejects control characters; it neither authenticates nor resolves a target. Private supervisor commands require explicit Job, Attempt and generation binding at the endpoint, with namespace owner and secret credentials separate from public host authentication tokens. Core also defines `HostVmCommand` (`Pause`, `Resume`, `Offload`, `Status`) and `HostVmResult` (`status`, `value`) for live VM controls. There is no `Load` wire operation: `--vm-load` selects `Resume`. Core defines structured commands/states/results only; transport, peer authentication and runtime controls remain outside core. `pvisor::host_vm_exchange` exchanges typed Host request/response envelopes for embedded callers.

## Local resource contracts

Core defines native CPU QoS and telemetry through `cpu`, `memory` and execution
contracts, not distributed scheduling or admission. `node::EnvironmentLayer`
identifies local image revisions with `handle` / `manifest_digest`; node image/RAM
ownership and snapshot implementation belong to `pvisor`. The separate
`pvisor-daemon` owns node-local sandbox admission and lifecycle.

```sh
just test core
```

[Operation and Event design](../../docs/src/zh/design/operations-events.md) · [System architecture](../../docs/src/zh/design/architecture.md)

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

`execution::RunProfile` is a typed execution request shared by frontends and
embedded callers. `RuntimeConfig.profile` defaults to `Default`; `Safe` requires
runtime sandbox admission before execution. It does not claim that every network
mechanism is non-bypassable. `PolicyMode::Enforce` and executor installation
observations retain that separate role.

Core defines native CPU QoS and telemetry through `cpu`, `memory` and execution
contracts, not distributed scheduling or admission. `node::EnvironmentLayer`
identifies local image revisions with `handle` / `manifest_digest`; node image/RAM
ownership and snapshot implementation belong to `pvisor`. The separate
`pvisor-daemon` owns node-local sandbox admission and lifecycle.

```sh
mise run test -- core
```

[Operation and Event design](../../docs/src/zh/design/operations-events.md) · [System architecture](../../docs/src/zh/design/architecture.md)

`NetworkPolicy::allows_unrestricted_packets` is a conservative admission predicate
for packet transports that cannot enforce proxy flow/port rules or bandwidth
budgets. It accepts only unrestricted effective policy; it does not authorize a
custom controller or prove that a driver installed controls. The peer transport
and owning runtime remain responsible for packet and lifecycle enforcement.

IP grants, explicit denials and resolved-address checks treat IPv4 and its
IPv4-mapped IPv6 representation as the same destination, including exact and
CIDR rules in either representation. Native IPv6 and IPv4-compatible IPv6 are
not converted to IPv4. Matching leaves the original connection address intact;
port and transport restrictions still apply.

An audit grant derives a request-scoped policy from the canonical capability;
it retains explicit denies and bandwidth limits, and does not mutate the source
policy or authorize other hosts, ports, transports, or private DNS results.

## Stage durability

`overlay::StageDurability` defaults to `Checkpoint`; `Strict` additionally persists
first-mutation observations. Explicit `Ephemeral` omits the automatic completion
data drain and offers no durable stage, apply, checkpoint or execution reuse.
It retains isolation, observation records and workload-requested synchronization.
This setting is not a global switch for all runtime or audit persistence.

### Optional stage observation recording

`StageRecording::{On, Off}` is serialized as `on`/`off`; omission defaults to
`On`. `OverlayConfig.recording = Off` requires `StageDurability::Ephemeral`.
It disables only per-file observations for apply/recovery. Normal file I/O,
access checks, explicit fsync, execution results and audit records remain.

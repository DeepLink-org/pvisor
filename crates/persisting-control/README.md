# persisting-control

**Shared Run contracts, authorization policies, AgentCtl messages, and event records.**

This dependency-light crate supplies the contracts shared by pVisor, Gateway,
and OverlayNet. It owns value types, policy decisions, the AgentCtl wire
protocol/client, and the EventRecord envelope. Runtime servers, executors, and
storage implementations stay in their respective components.

| Module | Responsibility |
| --- | --- |
| `runtime` | Run/Attempt identity, configuration, capabilities and results |
| `policy` | Network/model authorization and control transitions |
| `protocol` | Versioned AgentCtl requests, state reports and directives |
| `events` | Shared event identity, envelope and validation |
| `client` | Synchronous AgentCtl Unix-socket client |

Types remain re-exported at the crate root. Commands and events keep their
existing JSON formats and sequencing scopes; they do not share a new transport
or acknowledgement protocol.

AgentCtl is an optional, cooperative channel between pVisor
and a Run-local runtime client. It is not a sandbox, does not discover
processes or external effects, and is never enforcement evidence by itself.

pVisor owns the Run-scoped server and credential injection. OverlayNet and
Gateway apply policy decisions; they do not own this protocol.

```text
Requested -> Allowed / Denied -> Applied / Failed
```

- `ControlRequest` is a typed resource request (currently network or model).
- `ControlController` evaluates policy and returns the authorization transition.
- `ControlMachine` validates transitions and retains the state/history.
- `protocol` is the dependency-light request/response schema shared with
  pVisor's server.
- `AgentCtlClient` discovers the authenticated Unix endpoint from the
  environment and drives Session creation, periodic state synchronization, and
  checkpoint quiescence.

An `Applied { effect: Deny }` state means the driver successfully blocked an
operation. It does not mean that a proxy-based driver is non-bypassable.

The protocol has two requests: `Hello` authenticates and opens a Session;
`Sync` exchanges the client's current state for pVisor's current directive.
Clients report `active`, `idle`, or `quiesced { checkpoint_id }`. pVisor replies
with `continue`, `quiesce { checkpoint_id, deadline_unix_ms? }`, or
`shutdown { reason? }`. A checkpoint succeeds only after every Session frozen
into that checkpoint reports the matching quiesced state.

pVisor injects four Run-local variables: `PERSISTING_AGENTCTL_ENDPOINT`,
`PERSISTING_AGENTCTL_TOKEN`, `PERSISTING_AGENTCTL_VERSION` (exactly `1`), and
`PERSISTING_AGENTCTL_TRANSPORT` (currently `unix`). New integrations use only
`PERSISTING_AGENTCTL_*`. A future interactive login or terminal will use a
separately authorized Debug protocol; terminal byte streams, PTY resize, and
signals will not enlarge this compact Control protocol.

See [`src/protocol.rs`](src/protocol.rs) for the complete wire contract, JSON
examples, state semantics, typed errors, and safety boundary.

## Develop

```rust
use persisting_control::{AgentCtlClient, AgentCtlClientConfig, AgentState};

let Some(config) = AgentCtlClientConfig::from_current_environment("worker-1")? else {
    return Ok(()); // not running under pVisor
};
let mut client = AgentCtlClient::new(config);
let directive = client.connect()?;
let directive = client.sync(AgentState::Active)?;
# Ok::<(), anyhow::Error>(())
```

```bash
just test persisting-control
# or: just test control
```

## Links

- [pVisor isolation architecture](../../docs/src/en/design/isolation.md)
- [OverlayNet architecture](../../docs/src/en/design/overlaynet.md)
- [System architecture](../../docs/src/en/design/architecture.md)
- [`persisting-pvisor`](../persisting-pvisor/README.md)

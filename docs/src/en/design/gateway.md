# Gateway implementation

Gateway is a runtime driver embedded in pVisor for model routing and recording observed traffic. It starts and stops with a Run. The public CLI has no standalone Gateway daemon.

## Data path

```text
Agent → injected proxy or base URL → OverlayNet HTTP path
      → protocol adapter → capture engine → per-story serial actor → event sink
```

Protocol adapters convert supported requests/responses into the shared `pvisor-core` event vocabulary. The engine carries Run, Attempt, agent, session and story identities. Each story actor writes to the sink serially and updates its in-memory turn index only after a successful append.

Public capture uses `pvisor_core::event::Event` and the shared Journal. Mutable capture inputs serve conversation projection rather than a second formal event envelope. Drafts do not enter the fact log; Markdown parameters remain compatibility-only.

## Order and persistence

Journal positions express commit order; stable event IDs support idempotent retries; causal references express known dependencies. Run and embedded Gateway share a Journal. Story actors commit facts before updating Story/SessionIndex and notifying observers. Observer failure does not roll back committed facts.

The command WAL has been removed. Startup reconstructs projections from committed facts without replaying HTTP requests, repeating observer notifications or rewriting logs. The bounded input queue remains best effort; only a Journal receipt proves durability. Flush reports rejected/failed work; shutdown waits for consumers to release the Journal. Nonempty historical WALs prevent startup and must first be drained with an older version, avoiding silent migration data loss.

## Observation boundary

Capture covers clients using the injected proxy or base URL. Without executor network enforcement, direct sockets can bypass an explicit proxy. A captured response proves observation on that path, not the absence of other traffic.

Capture level controls retained payload. Full payloads can contain prompts, model output and request details. Run Bundle evidence and capture events are separate records: events do not contain every Bundle file change, output field or control observation.

## Code ownership

| Component | Source area | Responsibility |
| --- | --- | --- |
| Protocol parsing/forwarding | `pvisor-gateway` | Model protocol conversion and call observation |
| Engine and story actors | `pvisor-gateway/src/engine` | Journal commit, causal identity and turn projections |
| Event vocabulary | `pvisor-core` | Shared serialized records; runtime components own sink implementations and integration |
| Runtime integration | `pvisor` | Run lifecycle, routes, event sink and shutdown |
| Network path | `pvisor-overlaynet` | Proxy transport and policy integration |

See [Capture guide](../guides/capture.md) for usage, [OverlayNet](overlaynet.md) for network enforcement, [Operation and Event](operations-events.md) for event contracts and [Core architecture](architecture.md) for component ownership.

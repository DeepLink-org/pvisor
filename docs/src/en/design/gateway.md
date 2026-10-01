# Gateway implementation

Gateway is an embedded pVisor driver for routing model calls and recording observed traffic. It starts and stops with a Run. There is no standalone Gateway daemon in the public CLI.

## Data path

```text
Agent → injected proxy/base URL → OverlayNet HTTP path
      → protocol adapters → capture engine → per-story actor → event sink
```

The protocol adapters turn supported requests and responses into the shared `pvisor-control` record vocabulary. The engine carries Run, Attempt, agent, session and story identities. A per-story actor serializes sink writes and updates the in-memory turn index after an append succeeds.

The public capture output uses `trace::Event` and the shared Journal. Mutable
capture inputs are dialogue projection data, not a second event envelope.
Draft commands remain excluded from the fact log; the Markdown flag remains a
compatibility option.

## Ordering and persistence

Journal positions describe commit order. Stable event IDs survive retries;
causal links describe known dependencies. Run and embedded Gateway share a
journal. A per-story actor commits a fact before updating Story and SessionIndex
or notifying observers. Observer failure does not undo a committed fact.

The command WAL has been removed. Startup rebuilds projections from committed
facts without replaying HTTP requests, notifying observers again, or rewriting
the log. The bounded input queue remains best-effort: only a Journal receipt
proves persistence. Flush reports rejected or failed work, and shutdown waits
for consumers to release the journal. A nonempty historical WAL blocks startup
until drained with the previous version.

## Observation boundary

Capture sees clients that use the injected proxy or base URL. A direct socket can bypass an explicit proxy when the executor does not enforce a network boundary. A captured response proves observation on that path, not the absence of other traffic.

Capture levels select how much payload is retained. Full payloads can include user prompts, model output and request details. Run Bundle evidence and capture events are different records: events do not contain every filesystem effect, output field or control observation in the Bundle.

## Code ownership

| Component | Source area | Responsibility |
| --- | --- | --- |
| Protocol decoding and forwarding | `pvisor-gateway` | Translate model protocols and observe calls |
| Engine and story actors | `pvisor-gateway/src/engine` | Journal commits, causal identity and turn projections |
| Event vocabulary | `pvisor-control` | Shared serializable records and sink contract |
| Runtime integration | `pvisor` | Run lifecycle, route setup, event sink and shutdown |
| Network path | `pvisor-overlaynet` | Proxy transport and policy hooks |

Start with the [capture guide](../guides/capture.md). For network enforcement, see [OverlayNet](overlaynet.md); for execution records, see the [system architecture](architecture.md).

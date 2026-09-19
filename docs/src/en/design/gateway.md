# Gateway implementation

Gateway is an embedded pVisor driver for routing model calls and recording observed traffic. It starts and stops with a Run. There is no standalone Gateway daemon in the public CLI.

## Data path

```text
Agent → injected proxy/base URL → OverlayNet HTTP path
      → protocol adapters → capture engine → per-story actor → event sink
```

The protocol adapters turn supported requests and responses into the shared `persisting-control` record vocabulary. The engine carries Run, Attempt, agent, session and story identities. A per-story actor serializes sink writes and updates the in-memory turn index after an append succeeds.

The public capture output is EventRecord JSONL. Draft commands are currently ignored by the story actor. `--gateway-stream-markdown` is retained for compatibility but does not produce a live Markdown projection. Build derived views from the persisted records instead of relying on that flag.

## Ordering and persistence

Sequence numbers belong to their producer/session ordering scope. Preserve identity fields when combining streams; neither wall-clock timestamps nor a bare `seq` value defines a global order. `timestamp` and `timestamp_unix_ms` express the same observation time in two formats.

The capture engine has a bounded, asynchronous WAL submission path with background group commit. Queue acceptance is not a synchronous durable commit. Recovery can replay persisted, unacknowledged work; abrupt termination before a queued record reaches disk can still lose that record. Graceful shutdown flushes pending work. Inspect capture errors and dead letters when assessing completeness.

A sink append may fail after writing some bytes. Such an error has an unknown outcome unless the sink can prove rejection; treating every I/O error as a clean rejection can cause unsafe retries. Runtime JSONL files use owner-only permissions, including when reopening an existing file.

## Observation boundary

Capture sees clients that use the injected proxy or base URL. A direct socket can bypass an explicit proxy when the executor does not enforce a network boundary. A captured response proves observation on that path, not the absence of other traffic.

Capture levels select how much payload is retained. Full payloads can include user prompts, model output and request details. Run Bundle evidence and capture events are different records: events do not contain every filesystem effect, output field or control observation in the Bundle.

## Code ownership

| Component | Source area | Responsibility |
| --- | --- | --- |
| Protocol decoding and forwarding | `persisting-gateway` | Translate model protocols and observe calls |
| Engine and story actors | `persisting-gateway/src/engine` | Identity, ordering, WAL, append and turn state |
| Event vocabulary | `persisting-control` | Shared serializable records and sink contract |
| Runtime integration | `persisting-pvisor` | Run lifecycle, route setup, event sink and shutdown |
| Network path | `persisting-overlaynet` | Proxy transport and policy hooks |

Start with the [capture guide](../guides/capture.md). For network enforcement, see [OverlayNet](overlaynet.md); for execution records, see the [system architecture](architecture.md).

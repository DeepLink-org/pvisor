# Gateway implementation

Gateway is a runtime driver embedded in pVisor for model routing and recording observed traffic. It starts and stops with a Run. The public CLI has no standalone Gateway daemon.

The [sandbox daemon](daemon/index.md) does not embed this Gateway. Its execd/egress endpoint proxy forwards prepared OpenSandbox services; it does not perform model routing/capture, inference-idle pause coordination or CPU-reservation release. These are distinct data paths, even though both forward HTTP.

## Data path

```text
Agent → injected proxy or base URL → OverlayNet HTTP path
      → protocol adapter → capture engine → per-story worker/mailbox → Journal → projections/observers
```

Protocol adapters convert supported requests/responses into the shared `pvisor-core` event vocabulary. The engine carries Run, Attempt, agent, session and story identities. One worker and one bounded FIFO mailbox own each story's preparation, Journal commit and projection I/O. Direct `apply`, asynchronous capture and snapshot/barrier commands share that owner, rather than chaining a prepare queue and a second actor mailbox. Local story commands and Run enrichment use typed records/state; serialized actor messages remain boundary adapters, not the in-process hot path. The Run registry lock is held only for synchronous enrichment, never across a Journal wait or cross-story dispatch.

Public capture uses `pvisor_core::event::Event` and the shared Journal. Mutable capture inputs serve conversation projection rather than a second formal event envelope. Drafts do not enter the fact log; Markdown parameters remain compatibility-only.

## Delegated credential actions {#delegated-credential-actions}

Gateway checks the HTTP method and endpoint before selecting a model route or
resolving its credential. POST is allowed for Chat Completions, Messages,
Responses, Embeddings and token counting at their exact unversioned or
`/v1` paths. Native Gemini allows `models/{model}:generateContent`,
`:streamGenerateContent` and `:countTokens` under `/v1`, `/v1beta` or no version
prefix. GET `/models`, `/v1/models` and `/v1beta/models` returns the local model
configuration without contacting an upstream. One trailing slash is accepted.
Administrative endpoints, other methods, unknown paths, ambiguous escaped/dot/
repeated-slash paths and method/path override headers or query parameters
(`_method`, `method`, `path`, `url` and related spellings) are rejected. Ordinary
API query parameters such as `alt=sse` and `api-version` are preserved.

The protocol bridge result is checked again. Gemini uses the URI model as its
identity, rejects a conflicting body model and rewrites the URI when a route
forwards to another model. Model path segments accept ASCII letters, digits,
hyphens, underscores and dots, but cannot be `.` or `..`.

Clients must use these Gateway paths; arbitrary client prefixes that happened to
match a protocol suffix are no longer accepted. A trusted route's `upstream`
can still include a service prefix such as `/team/v1`. The configured upstream
must implement the advertised API semantics. The ordinary egress policy and
the explicit delegated model grant are separate: `no-network` can still permit
these model requests, and this action check does not make capture mandatory or
establish a spend budget.

The legacy Detect classification has no defined delegated model action and is
not on this allowlist. Realtime HTTP session/credential management is also not
delegated; WebSocket transport retains its explicit unsupported response.

## Order and persistence

Journal positions express commit order; stable event IDs support idempotent retries; causal references express known dependencies. Run and embedded Gateway share a Journal. Story workers commit facts before updating Story/SessionIndex and notifying observers. Observer failure does not roll back committed facts.

The command WAL has been removed. Startup reconstructs projections from committed facts without replaying HTTP requests, repeating observer notifications or rewriting logs. The bounded input queue remains best effort; only a Journal receipt proves durability. `spawn_apply` uses nonblocking bounded admission; direct `apply` waits for admission to the same FIFO. Acceptance is not durability, and cancellation after admission does not cancel the owned job. Flush waits behind accepted jobs and the backfills they await, reporting rejected/failed work; it neither fences producers still admitting new work nor acts as a rejected-event diagnostic-writer barrier. Subagent link backfills flow to the main story; same-story backfills execute directly under the owner instead of waiting on their own mailbox. A missing backfill receipt is a capture gap. Shutdown stops admission, drains accepted tails/backfills and persists final projections/index even when another story reports a gap; consumers then release the Journal.

Failed prepared backfills retain `prepared_story` (the target StoryContext) and `prepared_record_json`, including the stamped event ID and timestamp. Recovery submits that retained record to the target story's same bounded owner, bypassing preparation and Run re-enrichment so an already-matched link is not lost or assigned a fresh identity. Capture-level filtering and sensitive-body redaction still apply to retained payloads. Entries without `prepared_story` remain legacy source-event retries through ordinary `apply`; diagnostics are retry input, not proof of a Journal commit.

Shutdown explicitly awaits an ordered marker in the bounded rejected-event writer, even if other runtime clones keep it alive. The marker guarantees append attempts for diagnostics queued before it and reports writer I/O errors; it does not fsync the dead-letter file or establish durability. Diagnostics rejected by that queue or admitted after the marker are outside the guarantee. Ordinary `flush` does not wait for this marker.

## Observation boundary

Capture covers clients using the injected proxy or base URL. Without executor network enforcement, direct sockets can bypass an explicit proxy. A captured response proves observation on that path, not the absence of other traffic.

Capture level controls retained payload. Full payloads can contain prompts, model output and request details. Run Bundle evidence and capture events are separate records: events do not contain every Bundle file change, output field or control observation.

## Code ownership

| Component | Source area | Responsibility |
| --- | --- | --- |
| Protocol parsing/forwarding | `pvisor-gateway` | Model protocol conversion and call observation |
| Engine and story scheduling owners | `pvisor-gateway/src/engine` | Journal commit, causal identity and turn projections |
| Event vocabulary | `pvisor-core` | Shared serialized records; runtime components own sink implementations and integration |
| Runtime integration | `pvisor` | Run lifecycle, routes, event sink and shutdown |
| Network path | `pvisor-overlaynet` | Proxy transport and policy integration |

See [Capture guide](../guides/capture.md) for usage, [OverlayNet](overlaynet.md) for network enforcement, [Operation and Event](operations-events.md) for event contracts and [Core architecture](architecture.md) for component ownership.

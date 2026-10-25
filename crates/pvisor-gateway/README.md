# pvisor-gateway

**pVisor's built-in Agent protocol driver: LLM HTTP forwarding plus canonical
trajectory capture.**

Owns the application-level path from Agent/LLM HTTP exchanges to trajectory
events: protocol recognition and adaptation, upstream selection, run/session/
story/call correlation, canonical event emission, and live human-readable
projections.

Does not own the proxy data plane. [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)
owns proxy transport, access enforcement, and generic sink dispatch.

Capture remains the user-facing capability. It runs through `pvisor spawn`.
Gateway is an internal pVisor driver and a reusable crate, not a peer product
or standalone service.

Online network/model policy is fixed in memory at startup. Session `proxy.toml`
files are diagnostic snapshots, omit inline API keys, and cannot change policy or
credential routes. Embedded Attempts bind the capture root to a host-owned Run
identity; client headers identify sub-sessions within that root.

When pVisor owns upstream credentials, each Attempt injects a fresh local SDK
credential and requires it before model dispatch. Missing/foreign tokens receive
401. Gateway consumes that credential before forwarding and resolves the supplier
key from its frozen routes. This scopes access to the token holder; it does not
isolate an unsandboxed process from other processes running as the same host UID.
Embedded users can supply `InProcessRuntime.client_token` and `run_id` explicitly.
HTTP upstreams use a ten-minute read-inactivity timeout rather than a ten-minute
total-stream deadline, so active long streams can continue.

This crate implements `pvisor-overlaynet::OverlaySink`. Protocol rendering
and capture share one in-memory `LlmRequestEventPayload` (`llm/v1`). Provider
wire formats are never chained through Chat Completions as an intermediate
protocol. Derived trajectory views are not part of the online
protocol-conversion path.

## Host-observed inference waits

An embedding runtime can install an Attempt-bound
[`ModelWaitLifecycle`](src/model_wait.rs) through
`InProcessRuntime.model_wait`, or through pVisor's
`GatewayDriverConfig::model_wait`. Host installation observes every authorized
model request; Agent, rollout and model-client code need no special header or
wait-lifecycle call. An absent lifecycle preserves ordinary forwarding. The old
`x-pvisor-inference-idle` header is ignored and stripped, including false/invalid
or duplicate values: it grants no authority and cannot bypass Host observation.
A model request is only a wait hint. The native owner must independently obtain
guest-kernel idle evidence before pausing/offloading; network silence alone is
not evidence that tools or background tasks are idle.

Gateway reserves an obligation only after model/action authorization and
credential resolution, and awaits `enter` before dispatching upstream. For a
non-streaming reply it waits for the body or a read failure. For SSE the default
boundary is the first nonempty body chunk, EOF or read failure. A Host lifecycle
may instead supply `ModelWait::stream_buffer() -> Some(ModelStreamBuffer)` and
expose its latched wake through `wait_for_wake()`. The native adapter supplies
both from its Attempt coordinator; no guest/client cooperation header is used.

Each batch collection ends on byte limit, a fixed time limit starting at
successful headers for the first batch or after the previous batch drains,
Host wake, EOF or error. Heartbeats do not reset the deadline. The bounds are
1 byte..16 MiB and >0..60 seconds and are validated before lifecycle entry.
The default is disabled; a custom VM-owning lifecycle must override the wake
method to expose its independent timer/device/cancellation events. The native
adapter does so without blocking on a RAM transition. Gateway then awaits
`before_batch` for repeated streams (or `before_delivery` for one-shot waits),
including native writeback/publication and resume ACK, before returning headers/data; the collection deadline is not a delivery
latency guarantee and never renews a guest idle lease.

The copy buffer retains at most `max_bytes` raw bytes. One received transport
chunk can exceed the remaining capacity: copy only up to the cap, stop reading,
and retain its suffix for ordered replay. HTTP/TLS buffers, this transport
suffix, capture storage and per-request allocation overhead are outside that
payload limit; it is not a process/node memory cap. No background prefetch queue
is created. Replay delivers the prefix, suffix and any subsequent error once,
then uses the existing translation/capture and backpressure path. EOF/errors
are terminal and the source is not polled again. An error before any bytes keeps
normal pre-body failure behavior. Raw content, reasoning, tool calls and SSE
heartbeats are treated identically; only delivery timing and transport chunk
boundaries change. `[DONE]` is not a transport EOF or a separately parsed trigger.
A lifecycle may opt in with `repeat_stream_batches()`. It must implement
`before_batch` to resume and hold off rearming, `after_batch` to release that
hold, and consume generation-scoped wakes without replaying old signals. The
Gateway retains the request guard and reads bounded batches repeatedly. Each
batch passes through translation/capture and the bounded output queue. An
internal marker is acknowledged when the HTTP body writer consumes all preceding
queue items; this is not a TCP ACK or guest application ACK. Only then does
`after_batch` allow fresh idle detection. EOF drains the final batch and calls
`before_delivery` once; drop, shutdown or errors cancel the guard. No marker is
sent over HTTP. With no buffer policy, forwarding stays continuous after the
first chunk and the hold remains through EOF. Existing custom lifecycles default
to one-shot behavior. Supplier errors also cross the delivery barrier before
Gateway sends an error response.

The lifecycle must durably bind the wait to its lease, coordinate parallel
calls, retain pause ownership, reacquire CPU admission and confirm native
resume. It must never override a human pause or revive a stale lease. Gateway
does not alter scheduler reservations. A guard exists before either async
callback starts; entry/delivery failure, a dropped handler and Gateway shutdown
call its synchronous `cancel` once. Successful delivery disarms this cleanup.
Implementations must enqueue bounded, fenced cleanup without blocking the
Gateway's I/O thread, including when an async operation was interrupted after
an uncertain effect.

For compatibility, the existing explicit-declaration adapter remains available.
pVisor's optional
`GatewayDriverConfig::native_model_wait(NativeModelWait::Pause | Offload)` binds
it to a fresh Attempt on Linux x86_64; see the
[native wait contract](../pvisor/README.md#cooperative-native-model-waits-exp-003-m0).
That prototype retains resource admission and supports one trusted whole-guest
wait, with manual pause ownership and native ACK barriers. It does not provide
distributed wait ownership, CPU reservation release or full inference spooling.

Separately, the `pvisor` runtime supplies an opt-in Attempt-local adapter through
`GatewayDriverConfig::cooperative_model_wait`; see its
[contract and limits](../pvisor/README.md#cooperative-model-waits-opt-in-rust-api).
It requires the single-vCPU guest-kernel idle handoff before pause and optional
offload. Overlapping calls wake an owned pause and permanently disable automatic
transitions for that Attempt, while forwarding and response barriers continue.
The two built-in modes are mutually exclusive. The Host-observed mode needs no
Agent header; the legacy mode alone retains its explicit declaration gate. It
does not release scheduler reservations. Custom lifecycle implementations receive
unmarked calls by default and must handle concurrency; legacy embedders may
explicitly override `requires_idle_declaration`. See [probe scope and measurement limits](../pvisor/README.md#long-reasoning-engineering-probe).
`pvisor-daemon` does not currently integrate this Gateway lifecycle; its execd
proxy must not be described as native inference-wait coordination.

The controlled [`model_wait_http`](tests/model_wait_http.rs) tests cover HTTP
ordering and cancellation, not VM density, resource reclamation or daemon
integration. This interface alone does not provide RAM reclamation or networked
VM hibernation. See [Gateway design](../../docs/src/en/design/gateway.md)
and [daemon boundaries](../../docs/src/en/guides/daemon/boundaries.md).

A Chat Completions `[DONE]` marker alone is not proof that every
candidate finished: a missing `finish_reason` still fails terminal validation.
The HTTP regression checks both valid completion and this malformed-stream case.

## Capture admission budgets

Each capture runtime retains at most 256 story queues/workers until shutdown,
with the existing 256-slot FIFO per story. External admission is also limited
across stories to 1,024 queued, waiting-for-FIFO, or active jobs and 64 MiB of
accounted input. Internal backfills use a separate reserved budget of 256 jobs
and 32 MiB; rejected-event diagnostics have 256 jobs and 8 MiB. Byte accounting
includes context, raw bodies, semantic inputs and prepared commands, using a
conservative serialized-size multiplier plus per-job overhead. These are
admission limits, not an RSS limit: committed story projections and the run
registry can still grow over the lifetime of a runtime.

Global job/byte/story exhaustion fails immediately rather than waiting for
permits retained by a child awaiting a main-story backfill. Internal work still
uses the ordered story FIFO. Oversized/exhausted internal work returns an error
and retains the existing prepared-backfill dead-letter path; it cannot silently
skip a required receipt. Flush/stop barriers do not consume data-job budgets,
so overload cannot prevent draining accepted work. Existing constructors and
persisted schemas are unchanged; limits are currently fixed defaults.

`apply` returns admission errors explicitly; only per-story FIFO capacity is
backpressured. `spawn_apply` remains nonblocking and reports a capture gap through
`flush`/`shutdown` plus logging, with bounded best-effort dead letters. Diagnostic
budget exhaustion is logged; it does not clear the capture gap. Story exhaustion
also applies to replay into a runtime; opening more than 256 retained stories
fails explicitly rather than partially succeeding unnoticed.

## Develop

```bash
mise run test -- pvisor-gateway
# or: mise run test -- capture
cargo nextest run --locked -p pvisor-gateway --test llm_fixtures --test ag_fixture_tests
```

## Links

- [Gateway architecture](../../docs/src/zh/design/gateway.md)
- [Capture trajectories](../../docs/src/zh/guides/capture.md)
- [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)


### Capture retention

`summary` persists metadata and counts without message text. `dialogue` retains visible user/assistant text, without raw bodies or full semantic history. `full` retains parsed bodies and semantic request/response records. The same limits apply to dead letters and debug body previews; complete replay evidence requires `full`. Credential redaction applies at every level.

`ModelWaitLifecycle::reserve_download` optionally supplies the OverlayNet Host
wait hook for authorized GET/CONNECT traffic. The native adapter enables it with
`CooperativeModelWait.downloads`, reuses the model coordinator's idle evidence,
cooldown and delivery barriers, and needs no Agent header. Custom lifecycles
retain the default no-hook behavior. CONNECT is opaque transport evidence, not
recognition of model requests or TLS payload semantics.

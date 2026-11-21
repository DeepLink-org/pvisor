# pvisor-gateway

Network data-plane callers use `pvisor_overlaynet::api` and its declared traits;
policy records remain owned by `pvisor_core::network`.

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

Capture observers run after Journal commit. An observer failure is logged and
attempts a trajectory dead letter containing the already-retained, redacted
event; it does not undo or retry that append or fail an otherwise successful
HTTP call. Diagnostic-write failures are also warning-only. These post-commit
failures alone do not turn flush/shutdown into capture-admission gap errors.

Gateway does not bundle a mock upstream service; HTTP tests own their controlled
upstream fixtures.

Online network/model policy is fixed in memory at startup. Session `proxy.toml`
files are diagnostic snapshots, omit inline API keys, and cannot change policy or
credential routes. Embedded Executions bind the capture root to a host-owned Run
identity; client headers identify sub-sessions within that root.

When pVisor owns upstream credentials, each Execution injects a fresh local SDK
credential and requires it before model dispatch. Missing/foreign tokens receive
401. Gateway consumes that credential before forwarding and resolves the supplier
key from its frozen routes. This scopes access to the token holder; it does not
isolate an unsandboxed process from other processes running as the same host UID.
Embedded users can supply `InProcessRuntime.client_token` and `run_id` explicitly.
HTTP upstreams use a ten-minute read-inactivity timeout rather than a ten-minute
total-stream deadline, so active long streams can continue.
Shutdown allows one second for existing cancellation responses, then closes
remaining proxy and admin transport I/O before draining accepted
capture work. Partial request uploads, ordinary proxy upstream waits and clients
that stop reading cannot retain a connection indefinitely during Execution teardown.
Normal response framing, trailers and backpressure are unchanged.

Request uploads are bounded before capture admission. Exceeding the request-body
byte limit returns 413; a transport/body read failure returns 400 instead of
claiming an oversized payload. Neither rejection dispatches upstream nor creates
a capture or model-wait obligation. Failed readers release the incoming body.

This crate implements `pvisor_overlaynet::api::OverlaySink`; its single
`observe(ProxyEvent)` callback records proxy decisions and outcomes. Protocol rendering
and capture share one in-memory `LlmRequestEventPayload` (`llm/v1`). Provider
wire formats are never chained through Chat Completions as an intermediate
protocol. Derived trajectory views are not part of the online
protocol-conversion path.

Conversion regressions and fuzz targets use the same production path:
`understanding::understand_request` → `conversion::translate_request_for_bridge`,
and `conversion::translate_response_for_bridge` for non-streaming replies.
Gemini schema normalization and provider error envelopes are private helpers.

Semantic renderers keep Gemini request and usage objects as typed JSON maps;
content merging borrows the matching role/parts once. Cached content still
suppresses only system/tool declarations, and optional zero-valued usage details
retain their source-presence semantics. The crate root denies direct non-test
`unwrap`/`expect` throughout Gateway. This restriction does not prove the absence
of other panic sources; tests retain assertion-oriented unwrapping.

`tests/semantic_rendering_contracts.rs` exercises the production understanding
and translation entry points without HTTP services or model calls. It checks
cached-content fields, merged message boundaries, ordered tool results, complete
usage and the shared SSE usage encoding with fixed and generated inputs.

Messages streaming usage is an instance-owned JSON map. Updates retain omitted
fields, replace supplied fields (including zero), and replace nested objects
without a recursive merge. Non-object usage payloads retain the existing ignore
behavior; empty-usage event filtering is unchanged.
`tests/stream_usage_contracts.rs` checks exact passthrough bytes, Unicode/chunk
boundaries, usage isolation and rejection of incomplete/malformed terminal state.
Its generated inputs have bounded counts and require no HTTP service or model.

Usage extraction and `TokenUsage::merge` return `Result`; inferred totals and
all six accumulated counters use checked addition. Merge overflow leaves every
counter unchanged. Session accumulation checks usage before changing timestamps,
activity, model, provider or cost; overflow returns an error without dirtying
the index. No zero, wrapped or saturated count substitutes for an overflow.
Legacy extraction still preserves explicit nonzero totals; semantic response
understanding still floors totals at input plus output, but now rejects an
unrepresentable floor rather than saturating it.

An overflowing SSE usage update invalidates semantic completion, even if later
frames contain valid counters or a terminal event. Transparent routes still
forward the exact upstream bytes; translated routes return the decoding error.
Capture preparation propagates usage errors. Redacted dead-letter writing also
returns an error instead of retaining false zero usage or disallowed response
text. These failures do not authorize another model invocation. Token overflow
does not roll back already-committed Journal events; session index updates are
post-commit and can report failure independently of the retained response.
`tests/usage_contracts.rs` covers arithmetic boundaries, atomic merges and index
updates, provider conversion, transparent streams and generated counter sequences.

## Host-observed inference waits

An embedding runtime can install an Execution-bound
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
both from its Execution coordinator; no guest/client cooperation header is used.

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

The `pvisor` runtime supplies one built-in Execution-local adapter through
`GatewayDriverConfig::cooperative_model_wait`; see its
[contract and limits](../pvisor/README.md#cooperative-model-waits-opt-in-rust-api).
It requires the single-vCPU guest-kernel idle handoff before pause and optional
offload. Overlapping calls wake an owned pause and permanently disable automatic
transitions for that Execution, while forwarding and response barriers continue.
`Execution` owns status and native controls; the coordinator owns automatic-pause
arbitration. All authorized calls are observed; client headers cannot select or
bypass observation. The M0 native adapter, declaration parser and compatibility
switch are removed. Custom lifecycles must establish idleness and handle concurrency.
Resource reservations remain held. See [probe scope and measurement limits](../pvisor/README.md#long-reasoning-engineering-probe).
`pvisor-daemon` does not currently integrate this Gateway lifecycle; its execd
proxy must not be described as native inference-wait coordination.

The controlled [`model_wait_http`](tests/model_wait_http.rs) tests cover HTTP
ordering and cancellation, not VM density, resource reclamation or daemon
integration. This interface alone does not provide RAM reclamation or networked
VM hibernation. See [Gateway design](../../docs/src/en/design/gateway.md)
and [daemon boundaries](../../docs/src/en/design/daemon/index.md#integration).

A Chat Completions `[DONE]` marker alone is not proof that every
candidate finished: a missing `finish_reason` still fails terminal validation.
The HTTP regression checks both valid completion and this malformed-stream case.

## Capture admission budgets

Each capture runtime retains at most 256 story queues/workers until shutdown,
with the existing 256-slot FIFO per story. External admission is also limited
across stories to 1,024 queued, waiting-for-FIFO, or active sessions and 64 MiB of
accounted input. Internal backfills use a separate reserved budget of 256 sessions
and 32 MiB; rejected-event diagnostics have 256 sessions and 8 MiB. Byte accounting
includes context, raw bodies, semantic inputs and prepared commands, using a
conservative serialized-size multiplier plus per-session overhead. These are
admission limits, not an RSS limit: committed story projections and the run
registry can still grow over the lifetime of a runtime.

Global session/byte/story exhaustion fails immediately rather than waiting for
permits retained by a child awaiting a main-story backfill. Internal work still
uses the ordered story FIFO. Oversized/exhausted internal work returns an error
and retains the existing prepared-backfill dead-letter path; it cannot silently
skip a required receipt. Flush/stop barriers do not consume data-session budgets,
so overload cannot prevent draining accepted work. Existing constructors and
persisted schemas are unchanged; limits are currently fixed defaults.

`apply` returns admission errors explicitly; only per-story FIFO capacity is
backpressured. `spawn_apply` remains nonblocking and reports a capture gap through
`flush`/`shutdown` plus logging, with bounded best-effort dead letters. Diagnostic
budget exhaustion is logged; it does not clear the capture gap. Story exhaustion
also applies to replay into a runtime; opening more than 256 retained stories
fails explicitly rather than partially succeeding unnoticed.

Task-budget mutex poisoning permanently rejects new admissions for that budget;
a new runtime is required to obtain a healthy budget. Permit destruction does not
read or repair poisoned counters and cannot panic because of a poisoned lock,
including during unwinding. Healthy permits retain exact task/byte accounting.
Already accepted work still drains, and independent external/internal budgets
remain usable. Nonblocking rejection still reports a capture gap even when the
diagnostic budget also rejects its dead letter. Fault-injection and generated
operation-sequence tests live in `engine::apply_queue::tests`.

Scheduler poisoning closes both external and internal admission. Flush/shutdown
recover queue and worker ownership only to drain and join accepted work; they
still return the poison error and never clear it. A poisoned failure registry
also makes barriers fail, even if no error string survived. Request-correlation
and run-registry poisoning return capture errors rather than supplying partial
identities or fallback routes.

Diagnostic-thread creation errors propagate through the existing
`CaptureRuntime::new` result before projection rebuild. Startup failure may leave
newly created Journal storage on disk; it does not publish a runtime without a
writer. The writer exclusively owns its join handle, without a separate mutex.
Drain markers still report preceding append failures without stopping the writer;
the final owner sends Finish and joins, including during unwinding. Worker panic
causes a closed-writer or dropped-reply drain error; Drop joins without propagating
the worker panic. No filesystem-I/O deadline or stronger durability guarantee is
introduced. Tests inject spawn errors and worker exits, check drop-time joining,
and generate event/marker sequences against the persisted FIFO.

## Checked capture state

`SessionIndexHandle::record_request`, `set_active` and `snapshot` return `Result`.
Index reads, updates and persistence reject a poisoned lock; a clean dirty flag
does not suppress that failure. Request-count overflow returns an error before
changing session metadata. Admin endpoints return HTTP 500 for an unavailable
index instead of publishing an empty or potentially inconsistent snapshot.
Already-committed Journal events are not rolled back by projection errors.

Subagent extraction, route registration and enrichment return `Result` for
matcher initialization failure. Successful extraction retains its existing
sorting, ID validation and malformed-tool-JSON handling. Enrichment completes
fallible extraction before mutating records or the registry. Invalid model
authorization transitions propagate through the transport's existing HTTP 502
error path, without dispatching upstream or granting retry authority.

Client-identification caches remain optional: poison disables lookup or discards
the completed probe result instead of reusing suspect state. Reasoning scopes use
a typed agent/story/provider/upstream key, retaining the prior escaped-JSON byte
limit without fallible key serialization. Observation timestamps are obtained
directly from initialization, and loopback addresses use typed constructors.
These changes do not migrate Gateway's public API boundary or alter persisted
record schemas.

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

## Best-effort client identification

Model forwarding uses cached client metadata and schedules missing identification
on a blocking worker, with four concurrent probes per registry and duplicate
suppression. A first request can omit client metadata; subsequent requests can use
a completed lookup. Identification does not authorize requests.

Linux reads `/proc/net/tcp{,6}`, socket links under `/proc/<pid>/fd`, and bounded
`cmdline`, `stat` and `status` files directly; it never falls back to `lsof` or
`ps`. Each lookup has a one-second cooperative deadline, at most 8,192 directory
entries and 65,536 descriptor entries, with 4 MiB TCP-table and 1 MiB command-line
read limits. These bounds do not interrupt an individual blocked filesystem
operation. Inaccessible/disappearing processes, exhausted budgets and ambiguous
socket owners leave identification absent. PID start times are checked around
command-line reads. Visibility is limited to the Gateway's procfs/network namespace.
Effective UIDs use local `/etc/passwd` names or numeric UID fallback, avoiding
potentially blocking NSS/network lookups; this can change fingerprints for users
known only through NSS.

macOS and other Unix platforms retain bounded `lsof`/`ps` probes: they receive a
kill after one second and are reaped before releasing their worker slot;
uninterruptible kernel waits can delay reaping. Read-back metadata is capped at
1 MiB. Probe failure leaves metadata absent and does not fail model forwarding.

An embedded VM execution must set `InProcessRuntime.resolved_address_policy` to
`VmEgress`, including network-only executions with LLM dispatch disabled. pVisor's
execution driver supplies it automatically. HTTP and CONNECT then share Core's
VM hard-address constraints with direct virtio-net egress. The default remains
`HostConnectorAliases` for host proxies. This setting does not introduce policy
hot reload or restrict the operator's configured model-provider routes.

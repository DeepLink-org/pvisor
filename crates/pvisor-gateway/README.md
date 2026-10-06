# pvisor-gateway

**pVisor's built-in Agent protocol driver: LLM HTTP forwarding plus canonical
trajectory capture.**

Owns the application-level path from Agent/LLM HTTP exchanges to trajectory
events: protocol recognition and adaptation, upstream selection, run/session/
story/call correlation, canonical event emission, and live human-readable
projections.

Does not own the proxy data plane. [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)
owns proxy transport, access enforcement, and generic sink dispatch.

Capture remains the user-facing capability. It runs through `pvisor run`.
Gateway is an internal pVisor driver and a reusable crate, not a peer product
or standalone service.

This crate implements `pvisor-overlaynet::OverlaySink`. Protocol rendering
and capture share one in-memory `LlmRequestEventPayload` (`llm/v1`). Provider
wire formats are never chained through Chat Completions as an intermediate
protocol. Derived trajectory views are not part of the online
protocol-conversion path.

## Cooperative inference waits

An embedding runtime can install an Attempt-bound
[`ModelWaitLifecycle`](src/model_wait.rs) through
`InProcessRuntime.model_wait`, or through pVisor's
`GatewayDriverConfig::model_wait`. The agent opts an individual model call in
with `x-pvisor-inference-idle: true`, declaring that the whole guest is
quiescent while this call waits. Ordinary model requests cannot establish that
other tools or background tasks are idle. An absent or `false` declaration,
or an absent lifecycle, preserves ordinary forwarding. Duplicate and invalid
declarations are rejected; this local header never reaches the model supplier.

Gateway reserves an obligation only after model/action authorization and
credential resolution, and awaits `enter` before dispatching upstream. For a
buffered reply it waits for the body or a read failure; for SSE it waits for
the first nonempty body chunk, EOF or a read failure, rather than treating early
HTTP headers as a completed inference wait. It then awaits `before_delivery`
before returning response headers or data. The prefetched SSE chunk is fed
once into the existing translation/capture stream; subsequent chunks remain
streamed with the existing backpressure and capture limits. Supplier errors
also cross the delivery barrier before Gateway sends an error response.

The lifecycle must durably bind the wait to its lease, coordinate parallel
calls, retain pause ownership, reacquire CPU admission and confirm native
resume. It must never override a human pause or revive a stale lease. Gateway
does not alter scheduler reservations. A guard exists before either async
callback starts; entry/delivery failure, a dropped handler and Gateway shutdown
call its synchronous `cancel` once. Successful delivery disarms this cleanup.
Implementations must enqueue bounded, fenced cleanup without blocking the
Gateway's I/O thread, including when an async operation was interrupted after
an uncertain effect.

The retired VM Worker/Controller adapter and its `release_cpu_on_idle`
configuration are no longer available. Gateway retains the embedding interface,
not distributed wait ownership or an automatic native CPU-release adapter.
`pvisor-daemon` does not currently integrate this Gateway lifecycle; its execd
proxy must not be described as native inference-wait coordination.

The controlled [`model_wait_http`](tests/model_wait_http.rs) tests cover HTTP
ordering and cancellation, not VM density, resource reclamation or daemon
integration. This interface alone does not provide RAM reclamation or networked
VM hibernation. Any future adapter must supply the ownership, admission and
cleanup contracts above. See [Gateway design](../../docs/src/en/design/gateway.md)
and [daemon boundaries](../../docs/src/en/guides/daemon/boundaries.md).

## Develop

```bash
just test pvisor-gateway
# or: just test capture
cargo nextest run --locked -p pvisor-gateway --test llm_fixtures --test ag_fixture_tests
```

## Links

- [Gateway architecture](../../docs/src/zh/design/gateway.md)
- [Capture trajectories](../../docs/src/zh/guides/capture.md)
- [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)


### Capture retention

`summary` persists metadata and counts without message text. `dialogue` retains visible user/assistant text, without raw bodies or full semantic history. `full` retains parsed bodies and semantic request/response records. The same limits apply to dead letters and debug body previews; complete replay evidence requires `full`. Credential redaction applies at every level.

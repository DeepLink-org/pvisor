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

## Develop

```bash
just test pvisor-gateway
# or: just test capture
cargo nextest run --locked -p pvisor-gateway --test llm_fixtures --test ag_fixture_tests
```

## Links

- [Gateway architecture](../../docs/src/en/design/gateway.md)
- [Capture trajectories](../../docs/src/en/guides/capture.md)
- [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)

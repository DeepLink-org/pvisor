# Capture agent trajectories

Gateway capture is a Run driver: the Run starts and stops it, and there is no standalone Gateway command or daemon. [Capabilities and evidence](../concepts/capabilities-and-evidence.md) explains what capture can prove and what it does not enforce.

Install `pvisor` with the [installation guide](../start/installation.md). A real agent can be configured directly through `pvisor run`:

```bash
export DEEPSEEK_API_KEY=sk-...
pvisor run \
  --name deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  -- claude
```

pVisor starts an embedded Gateway, injects proxy or base-URL configuration into the child process, waits for execution, drains capture, and stops the Gateway. Use `--record-destination ./capture` to write the Trace Event journal to a chosen directory. `--gateway-stream-markdown` is kept only for compatibility and currently produces no Markdown projection.

### Event timestamps and order

The default output is `events.trace.jsonl`: it first writes a `pvisor.trace/5` header, then records containing an Event and its `{journal, offset}`. Observation time is `event.observed_at_unix_ms`; cross-producer causality uses `event.caused_by` and cannot be inferred from timestamps.

Gateway content lives in `event.data.payload.content`, story/session routing in `event.data.payload.story`, and call linkage in `event.data.payload.correlation`. A Run and its embedded Gateway share one Journal. Entering the queue is not persistence; `LocalSync` is returned only after the file is synchronized. Only formal Event Journals are supported; older JSONL is no longer read.

A client is observable only when it uses the injected proxy or base URL. Whether a direct socket is restricted depends on the executor, and the Run Bundle defines the actual isolation boundary.

# Capture agent trajectories

Gateway capture is a Run driver, started/stopped by its Run. There is no independent Gateway command/daemon. [Evidence](../concepts/capabilities-and-evidence.md) explains what capture proves and what it does not enforce.

[Install](../start/installation.md) pVisor, then configure the embedded Gateway:

```bash
export DEEPSEEK_API_KEY=sk-...
pvisor run \
  --name deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  -- claude
```

pVisor injects proxy/base URL configuration, waits for execution, drains capture, and stops Gateway. `--record-destination` writes a Trace Event Journal. `--gateway-stream-markdown` is compatibility-only and currently produces no Markdown projection.

### Event time and order

Default events.trace.jsonl begins with pvisor.trace/5, then records Event plus journal/offset. observed_at_unix_ms is observation time; caused_by encodes cross-producer causality, not inferred timestamps.

Gateway content is in event.data.payload.content; story/session routing in story; call linkage in correlation. Run and Gateway share a Journal. Queue acceptance is not persistence; LocalSync follows file synchronization. Only formal Event Journals are supported; old JSONL is rejected.

Only clients using the injected proxy/base URL are observed. Direct-socket constraints depend on the executor; Bundle evidence defines isolation.

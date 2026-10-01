# Capture Agent Trajectories

Gateway capture is a pVisor Run driver. It is started and stopped with the Run;
there is no standalone Gateway command or daemon. The
[capability and evidence model](../concepts/capabilities-and-evidence.md)
explains what capture proves and what it does not enforce.

Install `pvisor` using the [installation guide](../start/installation.md), then run a real Agent:

```bash
export DEEPSEEK_API_KEY=sk-...

pvisor run \
  --name deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  -- claude
```

pVisor starts the embedded Gateway, injects proxy/base-URL values into the
child, waits for the child, flushes capture, and stops the Gateway. Each Run
writes all metadata, trajectory, and optional filesystem state into the Run
record directory (or the explicit `--stage` directory).

Set `--record-destination ./capture` to write Trace Event journal to the specified
directory. `--gateway-stream-markdown` is a compatibility flag; the current
story actor does not produce a Markdown projection.

### Event timestamps and ordering

The default file is `events.trace.jsonl`: a `pvisor.trace/4` header followed by
records pairing an immutable `Event` with `{journal, offset}`. Observation time
is `event.observed_at_unix_ms`; it does not establish cross-producer order.
Causal links use `event.caused_by`.

Gateway content is in `event.data.payload.content`, with story/session routing
in `event.data.payload.story` and call IDs in `event.data.payload.correlation`.
Run and the embedded Gateway share one journal. Queue acceptance is not durable
acceptance; a `LocalSync` receipt follows filesystem synchronization. Legacy JSONL is rejected; only formal Event journals are supported.

Clients must use an injected proxy or base URL to be observed. Direct sockets
can bypass the explicit proxy unless the selected executor provides an enforced
network boundary; inspect the Run Bundle for the effective isolation level.

Next: read the [Gateway implementation](../design/gateway.md).

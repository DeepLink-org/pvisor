# System Design

Persisting provides durable infrastructure for Agent execution and trajectory
history. This section focuses on the
current public product path:

- [pVisor](../pvisor/index.md) virtualizes and governs one Agent Run.

Gateway, OverlayFS, and OverlayNet are pVisor runtime mechanisms. Where
available, stable Run identity connects the domains, but each also has a
standalone entry path.

![Persisting product domains and integration](../../assets/diagrams/persisting/system-products.svg)

## Cross-product contract

```text
pVisor Run
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> EventRecord JSONL (+ optional live Markdown)
  Run Bundle + staged Effects → review / apply / drop
```

Attempt finalization writes a private, versioned Run Bundle and leaves Effects
staged for later review/apply/drop. Configured capture writes Gateway trajectory
events and pVisor lifecycle records as EventRecord JSONL with the Run,
including the Evidence those records carry. The full Bundle and its Artifact,
lineage, Effect, and broader Evidence inventory remain local unless moved
separately.

The ownership boundary is deliberately simple:

- **pVisor owns execution.** It defines one Run's boundary and its model,
  network, and filesystem runtime drivers. Its private Run Bundle remains the
  execution record.

## Continue by question

- [Complete architecture and target model](architecture.md)
- [Local-to-fleet continuity](local-to-fleet.md)
- [Security and evidence model](security-evidence.md)
- [pVisor implementation boundaries](../pvisor/design/index.md)

Delivery state is reported in the product Design pages and
[Project Engineering Notes](../project/engineering.md). Target architecture is
not evidence that a capability is implemented.

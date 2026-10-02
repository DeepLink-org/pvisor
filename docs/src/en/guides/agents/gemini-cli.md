---
status: todo
search:
  exclude: true
---

# Gemini CLI

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How can Gemini CLI run within pVisor with controlled file and network access?

## Requirements

- Metrics: reproducible setup, correct injection, observed controls.
- Control: agent-native sandbox or approval mode.
- Workload: read/write/model-API task.
- Environment: pinned agent version and platform.

## Acceptance criteria

- Copyable commands and verification.
- Injection method and limitations.
- Pinned supported version with regression coverage.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Agents](index.md), [platforms](../../reference/platforms.md)

## Integration starting point using the existing CLI

Install Gemini CLI inside the selected executor and verify it in an independent test project. The destination below is generativelanguage.googleapis.com:443. For aider/OpenCode, first select the corresponding OpenAI provider in the agent configuration; this command does not configure providers for you.

```bash
pvisor run --safe --overlaynet-allow generativelanguage.googleapis.com:443 \
  --pass-env GEMINI_API_KEY -- gemini
pvisor status --review last
pvisor apply last --path src
```

Explicit allow replaces safe preset lists. Add authentication/provider/dependency destinations individually. Only the direct executable name selects adaptation; shell wrappers change matching. Safe HOME writes are discarded, so do not depend on persistent login state from this run.

These are existing pVisor options, not a completed version-pinned integration regression. Verify staging/denial/selective apply with [first run](../../start/first-run.md), then inspect real-agent Bundle evidence. Linux selective host proxies remain cooperative; use a VM containing the agent for mandatory enforcement.

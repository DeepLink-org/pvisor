# Threat model

pVisor protects the project workspace, HOME, credentials, network, and model quota. It assumes workloads **may fail or be manipulated**, and it trusts the supervisor process, the host kernel, and the executor backend.

Per-executor, per-capability protection is in [executor boundaries](executor-boundaries.md).

## Assets

| Asset | Typical risk |
| --- | --- |
| Project workspace | Accidental deletion, corruption, or overwrite by unreviewed changes |
| Your manual edits during a run | Overwritten by agent changes |
| HOME and application state | Configuration rewritten, state polluted |
| Credentials (SSH keys, cloud credentials, API keys) | Read or exfiltrated |
| Other host paths | Read or written |
| Network | Unauthorized destinations, data exfiltration |
| Model quota | Misuse |

## Adversaries and mistakes

1. **A mistaken agent** misunderstands the task and deletes or rewrites files it should not touch.
2. **A prompt-injected agent** is induced by repository content, web pages, or tool output to read secrets or reach external addresses.
3. **A malicious dependency or tool** performs extra work when the agent installs or invokes it.
4. **A malicious or abnormal model response** requests tool calls beyond what the task needs.

## Trust boundary

```text
You (review and apply)
  └─ pVisor supervisor (trusted): admission, staging, apply, evidence
       └─ Executor boundary: host platform controls / OCI container / libkrun VM
            └─ Workload (untrusted): agent, script, the child processes it starts
```

- Workspace changes must cross the **staging and apply** boundary to reach the project; see [staging and apply semantics](../concepts/staging.md).
- File reads, network, and process constraints come from the **executor boundary**, and their strength varies by executor and platform.
- The supervisor produces the records, so the workload cannot rewrite its own Run Bundle.

## Out of scope

- Bugs in the host kernel, the virtualization layer, or the FUSE implementation.
- Side channels (timing, cache, resource contention).
- Misuse of explicitly granted credentials: a key handed to the agent through `--pass-env` can be used any way the agent likes.
- Exfiltration through authorized destinations: domain rules cannot tell inference, telemetry, and upload APIs on the same domain apart, nor stop content smuggled inside model requests.
- Hostile multi-tenancy: local records serve review and diagnostics and provide no cryptographic proof; on macOS the VMM still holds the calling user's host permissions.
- Rolling back external effects: staging covers workspace files only.

## What evidence is for

A claim is not evidence. An admission-time plan can be at most `Planned`; actual enforcement comes only from the observations the executor returns at teardown. A security review should therefore read the Run Bundle, not the configuration. See [capabilities, evidence, and assurance boundaries](../concepts/capabilities-and-evidence.md).

## Apply the model to a real review

List the assets the task touches, then choose a control for each: staging and conflict checks for project files, an executor for reads outside the view, the matching network boundary for ordinary egress, and Gateway or a short-lived explicit grant for model keys. External writes that must be allowed need their own service-side permissions and review.

Record "request → plan → installed control → observation → uncovered part". When an observation is missing, mark the conclusion unknown; when only a cooperative control is available, describe how the workload bypasses it. Never treat an untriggered tool, an unreachable network, or a non-adversarial sample as proof that a boundary holds.

# Threat model

This page defines what can go wrong, the protected assets, and trust boundaries. See [executor boundaries](executor-boundaries.md) for per-platform capabilities.

## Assets

| Asset | Risk |
| --- | --- |
| Project | Deletion, corruption, unreviewed changes |
| Concurrent manual edits | Overwritten by agent changes |
| HOME/application state | Configuration/state contamination |
| SSH/cloud/API credentials | Reading or exfiltration |
| Other host paths | Reads or writes |
| Network | Unauthorized destinations/exfiltration |
| Model quotas | Misuse |

## Adversaries and mistakes

Workloads **may fail or be manipulated**:

1. Agents misunderstand tasks and edit/delete wrong files.
2. Prompt injection in repositories/pages/tool output induces secrets access or unwanted requests.
3. Malicious dependencies/tools perform additional actions.
4. Malicious/abnormal model responses request unnecessary tools.

The **supervisor, host kernel, and executor backend are trusted**.

## Trust boundary

```text
You (review and apply)
  └─ Trusted pVisor supervisor: admission, staging, apply, evidence
       └─ Executor boundary: host controls / OCI / libkrun VM
            └─ Untrusted workload: agent, script, descendants
```

Staged workspace changes cross apply before reaching the project; see [staging](../concepts/staging.md). Executors supply file-read/network/process controls with different strengths. The trusted supervisor produces records; workloads cannot rewrite their own Bundle within the configured boundary.

## Exclusions

- Host kernel, virtualization, and FUSE vulnerabilities.
- Timing/cache/resource side channels.
- Misuse of credentials explicitly granted through `--pass-env`.
- Exfiltration through authorized destinations: domain rules do not distinguish inference/telemetry/uploads or data embedded in prompts.
- Hostile multi-tenancy/cryptographic proof; macOS VMM retains caller host permissions.
- Rolling back external effects; staging covers workspace files only.

## Evidence

Plans stop at Planned; enforcement comes from executor teardown observations. Review Bundles rather than configuration alone. See [evidence](../concepts/capabilities-and-evidence.md).

## Apply the model to a review

List task assets and controls: staging/conflicts for project files, executor for outside reads, appropriate network boundary for egress, Gateway or short-lived explicit grants for model keys. External writes also need service-side permissions/review.

Record request → plan → installed control → observation → exclusions. Missing observation means unknown. Cooperative controls need explicit bypass assumptions. Untriggered tools, unavailable networking, or nonadversarial samples do not establish a boundary.

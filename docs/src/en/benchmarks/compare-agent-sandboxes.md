# When is an Agent sandbox enough, and when does pVisor help?

## Main conclusions {#conclusions}

**For one Agent, its built-in sandbox directly manages tool permissions; pVisor suits shared staging, review, conflict protection and records across Agents.** They can be combined, but default nested-sandbox compatibility and full overhead are unmeasured here.

Controlled staged Claude/Codex loops take **1.07/2.25 s**, near native **0.82/1.97 s**. VM Codex takes **10.93 s**, while Claude initialization times out. This compares execution environments, not built-in sandbox product speed.

| Need | Selection implication |
|---|---|
| One Agent’s tool permissions | Its built-in sandbox may suffice |
| Shared review protocol across Agents | Evaluate pVisor |
| Default nested sandboxes | Require separate compatibility tests |

## Motivation {#motivation}

Permissions control whether actions execute; review/application controls when changes reach originals. Those needs determine whether to use built-in sandboxes, independent worktrees or pVisor stage.

## Experiment design {#interpretation}

Capabilities come from official documentation. Measurements pin CLI versions, tools, controlled responses and repair plans, with 30 trials per available cell. Codex uses inner `danger-full-access`; the outer environment supplies its stated boundary. Default built-in sandboxes are not compared. Gemini CLI has no measured local task.

| Option | Permission and workspace approach |
|---|---|
| Claude Code | Sandboxed Bash and children have file/network restrictions; other tools have separate permissions |
| Codex | Sandbox boundaries and approvals are separate settings; workspace-write directly edits permitted workspaces |
| Gemini CLI | OS or container sandbox options; Docker/Podman mounts the workspace |
| pVisor | Host/isolated host/OCI/VM choices; stage retains changes with preimage checks at apply |

Sources: [Claude Code](https://code.claude.com/docs/en/sandboxing), [Codex](https://developers.openai.com/codex/security/), [Gemini CLI](https://geminicli.com/docs/cli/sandbox/). pVisor boundaries are in [isolation validation](isolation-tests.md). Current documented capabilities and pinned measured versions are distinguished.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

### Measured CLI loops {#reference-comparison}

Pinned clients in one tool environment; P50 seconds, N=30 and 3 warmups per available cell, 2026-10-04. This does not compare default built-in sandboxes.

| Execution environment | Claude tool loop | Codex tool loop |
|---|---:|---:|
| Native | 0.82 | 1.97 |
| pVisor host staged | 1.07 | 2.25 |
| pVisor VM | FAILED / N=0 | 10.93 |
| Docker rootless | 1.23 | 6.26 |
| Firecracker PCI | 3.03 | 7.83 |
| QEMU q35 | 2.67 | 7.69 |
| QEMU microvm | 2.71 | 7.67 |

Claude passes 30/30 on native/staged/Docker/reference VMs; pVisor VM initialization exceeds 90 s, formal N=0. Codex passes 30/30 across all eight groups. Fixed responses exclude inference; results establish only the specific tool paths and versions.

Staging supports review before application: apply/drop paths selectively, rejecting conflicts when the host changes the same file. Built-in sandboxes can also use Git/worktrees for review without pVisor's protocol. Standalone stage does not automatically restrict outside-view host access.

[Task data and compatibility](agent-tasks.md#reference-env) · [Apply costs](apply.md) · [Executor boundaries](../guides/executors/index.md)

### Downloads and reproduction {#run}

[Derived table CSV](compare-agent-sandboxes.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

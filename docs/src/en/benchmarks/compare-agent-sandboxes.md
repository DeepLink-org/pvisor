# Comparison: built-in Agent sandboxes

## Main conclusions {#conclusions}

**For one Agent, its built-in sandbox directly manages tool permissions; pVisor suits shared staging, review, conflict protection and records across Agents.** They can be combined, but default nested-sandbox compatibility and full overhead are unmeasured here.

Controlled staged Claude/Codex loops take **1.07/2.25 s**, near native **0.82/1.97 s**. VM Codex takes **10.93 s**, while Claude initialization times out. This compares execution environments, not built-in sandbox product speed.

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

Sources: [Claude Code](https://code.claude.com/docs/en/sandboxing), [Codex](https://learn.chatgpt.com/docs/sandboxing), [Gemini CLI](https://geminicli.com/docs/cli/sandbox/). pVisor boundaries are in [isolation validation](isolation-tests.md). Current documented capabilities and pinned measured versions are distinguished.

## Data and analysis {#results}

### Measured CLI loops {#reference-comparison}

Claude passes 30/30 on native/staged/Docker/reference VMs; pVisor VM initialization exceeds 90 s, formal N=0. Codex passes 30/30 across all eight groups. Fixed responses exclude inference; results establish only the specific tool paths and versions.

Staging supports review before application: apply/drop paths selectively, rejecting conflicts when the host changes the same file. Built-in sandboxes can also use Git/worktrees for review without pVisor's protocol. Standalone stage does not automatically restrict outside-view host access.

[Task data and compatibility](agent-tasks.md#reference-env) · [Apply costs](apply.md) · [Executor boundaries](../guides/executors/index.md)
